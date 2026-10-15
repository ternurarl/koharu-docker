use std::collections::BTreeMap;

use koharu_scene::EntityId;

use crate::Stage;

/// How many jobs of one stage may be in flight at once.
///
/// Local stages stay at one: running several pages through the same model at
/// once multiplies model residency, and multi-page concurrency is out of scope
/// for the stage overlap work (`plan/pipeline-stage-overlap-plan.md`).
///
/// Translation is the exception. A remote request holds neither a local model
/// nor the accelerator lane, so two in flight at once only bounds how many
/// remote translation requests may be outstanding at the same time. What the
/// configured provider does under that load — rate limiting above all — is
/// unverified.
const TRANSLATION_CONCURRENCY: usize = 2;

const fn concurrency(stage: Stage) -> usize {
    match stage {
        Stage::Detection | Stage::Ocr | Stage::Inpainting => 1,
        Stage::Translation => TRANSLATION_CONCURRENCY,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WorkState {
    Pending,
    Running,
    Finished,
}

struct StageWork {
    stage: Stage,
    state: WorkState,
}

struct PageWork {
    page: EntityId,
    stages: Vec<StageWork>,
}

impl PageWork {
    fn finished(&self) -> bool {
        self.stages
            .iter()
            .all(|work| work.state == WorkState::Finished)
    }

    /// Whether this page's own prerequisite for `index` is satisfied.
    ///
    /// A prerequisite that is not part of the requested stages counts as
    /// satisfied, so `Operation::Only { stage }` runs do not wait for stages
    /// that were never requested.
    fn ready(&self, index: usize) -> bool {
        let Some(prerequisite) = prerequisite(self.stages[index].stage) else {
            return true;
        };
        self.stages
            .iter()
            .find(|work| work.stage == prerequisite)
            .is_none_or(|work| work.state == WorkState::Finished)
    }
}

pub(crate) struct Scheduler {
    pages: Vec<PageWork>,
    page_index: BTreeMap<EntityId, usize>,
    total: usize,
}

impl Scheduler {
    pub(crate) fn new(pages: &[EntityId], stages: &[Stage]) -> Self {
        let pages = pages
            .iter()
            .map(|page| PageWork {
                page: *page,
                stages: stages
                    .iter()
                    .map(|stage| StageWork {
                        stage: *stage,
                        state: WorkState::Pending,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let total = pages.len().saturating_mul(stages.len());
        Self {
            page_index: pages
                .iter()
                .enumerate()
                .map(|(index, page)| (page.page, index))
                .collect(),
            pages,
            total,
        }
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }

    /// Dispatches the next ready `(page, stage)` work item, if there is one.
    ///
    /// Readiness is a property of the work item alone:
    ///
    /// 1. it is still `Pending`;
    /// 2. its stage is below its concurrency limit in `running`;
    /// 3. its page-local prerequisite is finished (`PageWork::ready`);
    /// 4. the Detection barrier has lifted.
    ///
    /// The barrier keeps Detection from ever overlapping another stage: while
    /// any Detection work is open, nothing else may start. `stage_finished` is
    /// vacuously true when Detection is not among the requested stages, so a
    /// run without Detection has no barrier at all.
    ///
    /// Stages are scanned in the order the requested stage list gives them —
    /// `Operation::stages` returns that list sorted, so Detection goes first
    /// and Ocr before Inpainting — and pages in page order within a stage.
    pub(crate) fn start_next(
        &mut self,
        running: &BTreeMap<Stage, usize>,
    ) -> Option<(EntityId, Stage)> {
        let stage_count = self.pages.first().map(|page| page.stages.len())?;
        for stage_index in 0..stage_count {
            let stage = self.pages[0].stages[stage_index].stage;
            if running.get(&stage).copied().unwrap_or(0) >= concurrency(stage) {
                continue;
            }
            if stage != Stage::Detection && !self.stage_finished(Stage::Detection) {
                continue;
            }
            for page_index in 0..self.pages.len() {
                if self.pages[page_index].stages[stage_index].state != WorkState::Pending
                    || !self.pages[page_index].ready(stage_index)
                {
                    continue;
                }
                let page = self.pages[page_index].page;
                self.pages[page_index].stages[stage_index].state = WorkState::Running;
                return Some((page, stage));
            }
        }
        None
    }

    pub(crate) fn complete_stage(&mut self, page: EntityId, stage: Stage) -> bool {
        let Some(&page_index) = self.page_index.get(&page) else {
            return false;
        };
        let page = &mut self.pages[page_index];
        let was_finished = page.finished();
        if let Some(work) = page.stages.iter_mut().find(|work| work.stage == stage) {
            work.state = WorkState::Finished;
        }
        !was_finished && page.finished()
    }

    /// Closes every remaining work item of `stage`.
    ///
    /// A run that stops or fails leaves its open work items `Running`, and the
    /// release drain only releases stages whose work has finished. This is what
    /// closes such a stage so its model can still be released at the end.
    pub(crate) fn finish_stage(&mut self, stage: Stage) {
        let Some(stage_index) = self
            .pages
            .first()
            .and_then(|page| page.stages.iter().position(|work| work.stage == stage))
        else {
            return;
        };
        for page in &mut self.pages {
            page.stages[stage_index].state = WorkState::Finished;
        }
    }

    pub(crate) fn stage_finished(&self, stage: Stage) -> bool {
        self.pages.iter().all(|page| {
            page.stages
                .iter()
                .find(|work| work.stage == stage)
                .is_none_or(|work| work.state == WorkState::Finished)
        })
    }
}

/// The only guard that keeps Translation behind Ocr for the same page.
///
/// The patch layer does not enforce this: a translation patch built before the
/// ocr patch commits applies cleanly and leaves a translation of the previous
/// source text. Do not relax this to overlap the translation request with the
/// ocr stage.
const fn prerequisite(stage: Stage) -> Option<Stage> {
    match stage {
        Stage::Detection => None,
        Stage::Ocr | Stage::Inpainting => Some(Stage::Detection),
        Stage::Translation => Some(Stage::Ocr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(count: usize) -> Vec<EntityId> {
        (0..count).map(|_| EntityId::new()).collect()
    }

    /// Marks a dispatched job as complete, as `Execution` does when one returns.
    fn finish(
        scheduler: &mut Scheduler,
        running: &mut BTreeMap<Stage, usize>,
        page: EntityId,
        stage: Stage,
    ) {
        scheduler.complete_stage(page, stage);
        if let Some(count) = running.get_mut(&stage) {
            *count = count.saturating_sub(1);
        }
    }

    #[test]
    fn detection_is_a_barrier_for_every_other_stage() {
        let pages = pages(2);
        let mut scheduler = Scheduler::new(&pages, &Stage::ALL);
        let mut running = BTreeMap::new();

        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[0], Stage::Detection))
        );
        running.insert(Stage::Detection, 1);
        // Detection runs alone and one page at a time.
        assert_eq!(scheduler.start_next(&running), None);

        finish(&mut scheduler, &mut running, pages[0], Stage::Detection);
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[1], Stage::Detection))
        );
        running.insert(Stage::Detection, 1);
        // The last open detection work still holds the barrier.
        assert_eq!(scheduler.start_next(&running), None);

        finish(&mut scheduler, &mut running, pages[1], Stage::Detection);
        assert!(scheduler.stage_finished(Stage::Detection));
        assert_eq!(scheduler.start_next(&running), Some((pages[0], Stage::Ocr)));
    }

    #[test]
    fn ocr_and_inpainting_overlap_on_one_page() {
        let pages = pages(1);
        let stages = [Stage::Detection, Stage::Ocr, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let mut running = BTreeMap::new();

        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[0], Stage::Detection))
        );
        running.insert(Stage::Detection, 1);
        finish(&mut scheduler, &mut running, pages[0], Stage::Detection);

        // Both siblings dispatch before either of them completes: inpainting
        // only needs detection, never the OCR whose result translation wants.
        assert_eq!(scheduler.start_next(&running), Some((pages[0], Stage::Ocr)));
        running.insert(Stage::Ocr, 1);
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[0], Stage::Inpainting))
        );
        running.insert(Stage::Inpainting, 1);
        // Translation still waits for this page's OCR.
        assert_eq!(scheduler.start_next(&running), None);
    }

    #[test]
    fn translation_waits_only_for_its_own_page_ocr() {
        let pages = pages(2);
        let stages = [Stage::Ocr, Stage::Translation];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let mut running = BTreeMap::new();

        assert_eq!(scheduler.start_next(&running), Some((pages[0], Stage::Ocr)));
        running.insert(Stage::Ocr, 1);
        assert_eq!(scheduler.start_next(&running), None);
        finish(&mut scheduler, &mut running, pages[0], Stage::Ocr);

        assert_eq!(scheduler.start_next(&running), Some((pages[1], Stage::Ocr)));
        running.insert(Stage::Ocr, 1);
        // Page 0's translation goes out while page 1 is still being OCRed.
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[0], Stage::Translation))
        );
        running.insert(Stage::Translation, 1);
        assert_eq!(scheduler.start_next(&running), None);

        finish(&mut scheduler, &mut running, pages[1], Stage::Ocr);
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[1], Stage::Translation))
        );
    }

    #[test]
    fn translation_concurrency_is_capped() {
        let pages = pages(3);
        let stages = [Stage::Ocr, Stage::Translation];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let mut running = BTreeMap::new();

        for page in &pages {
            assert_eq!(scheduler.start_next(&running), Some((*page, Stage::Ocr)));
            finish(&mut scheduler, &mut running, *page, Stage::Ocr);
        }

        for page in pages.iter().take(TRANSLATION_CONCURRENCY) {
            assert_eq!(
                scheduler.start_next(&running),
                Some((*page, Stage::Translation))
            );
            *running.entry(Stage::Translation).or_default() += 1;
        }
        assert_eq!(scheduler.start_next(&running), None);

        finish(&mut scheduler, &mut running, pages[0], Stage::Translation);
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[2], Stage::Translation))
        );
    }

    #[test]
    fn local_stages_stay_at_concurrency_one() {
        let pages = pages(2);
        let stages = [Stage::Ocr, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let mut running = BTreeMap::new();

        assert_eq!(scheduler.start_next(&running), Some((pages[0], Stage::Ocr)));
        running.insert(Stage::Ocr, 1);
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[0], Stage::Inpainting))
        );
        running.insert(Stage::Inpainting, 1);
        // Neither local model takes a second page at once.
        assert_eq!(scheduler.start_next(&running), None);

        finish(&mut scheduler, &mut running, pages[0], Stage::Ocr);
        assert_eq!(scheduler.start_next(&running), Some((pages[1], Stage::Ocr)));
        running.insert(Stage::Ocr, 1);
        assert_eq!(scheduler.start_next(&running), None);

        finish(&mut scheduler, &mut running, pages[0], Stage::Inpainting);
        assert_eq!(
            scheduler.start_next(&running),
            Some((pages[1], Stage::Inpainting))
        );
    }

    #[test]
    fn a_requested_stage_list_without_detection_has_no_barrier() {
        let both = pages(2);
        let mut scheduler = Scheduler::new(&both, &[Stage::Inpainting]);
        assert_eq!(
            scheduler.start_next(&BTreeMap::new()),
            Some((both[0], Stage::Inpainting))
        );

        // Inpainting is a sibling of translation: it only needs detection, and
        // detection was not requested.
        let page = pages(1);
        let stages = [Stage::Ocr, Stage::Translation, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&page, &stages);
        let mut running = BTreeMap::new();
        assert_eq!(scheduler.start_next(&running), Some((page[0], Stage::Ocr)));
        running.insert(Stage::Ocr, 1);
        assert_eq!(
            scheduler.start_next(&running),
            Some((page[0], Stage::Inpainting))
        );
    }

    #[test]
    fn complete_stage_reports_the_page_that_just_finished() {
        let pages = pages(1);
        let stages = [Stage::Detection, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);

        assert!(!scheduler.complete_stage(pages[0], Stage::Detection));
        assert!(scheduler.complete_stage(pages[0], Stage::Inpainting));
        // A page that is already complete does not report twice.
        assert!(!scheduler.complete_stage(pages[0], Stage::Inpainting));
    }

    #[test]
    fn finish_stage_closes_the_remaining_work_of_one_stage() {
        let pages = pages(2);
        let stages = [Stage::Detection, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);

        assert_eq!(
            scheduler.start_next(&BTreeMap::new()),
            Some((pages[0], Stage::Detection))
        );
        // Stopping mid-batch closes the work that never ran, which is what lets
        // the release drain see the stage as finished.
        scheduler.finish_stage(Stage::Detection);
        assert!(scheduler.stage_finished(Stage::Detection));
        assert_eq!(
            scheduler.start_next(&BTreeMap::new()),
            Some((pages[0], Stage::Inpainting))
        );
    }
}
