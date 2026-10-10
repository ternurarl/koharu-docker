use std::collections::{BTreeMap, BTreeSet};

use koharu_scene::EntityId;

use crate::Stage;

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
    active_stage: usize,
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
            active_stage: 0,
            total,
        }
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }

    pub(crate) fn start_next(
        &mut self,
        busy_stages: &BTreeSet<Stage>,
    ) -> Option<(EntityId, Stage)> {
        let stage_count = self.pages.first().map(|page| page.stages.len())?;
        while self.active_stage < stage_count {
            let stage_index = self.active_stage;
            let mut stage_finished = true;
            for page_index in 0..self.pages.len() {
                let work = &self.pages[page_index].stages[stage_index];
                if work.state != WorkState::Finished {
                    stage_finished = false;
                }
                if work.state == WorkState::Pending
                    && !busy_stages.contains(&work.stage)
                    && self.pages[page_index].ready(stage_index)
                {
                    let page = &mut self.pages[page_index];
                    let work = &mut page.stages[stage_index];
                    work.state = WorkState::Running;
                    return Some((page.page, work.stage));
                }
            }
            if !stage_finished {
                return None;
            }
            self.active_stage += 1;
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
        if self.active_stage == stage_index {
            self.active_stage += 1;
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

    #[test]
    fn starts_one_stage_batch_before_the_next_stage() {
        let pages = pages(2);
        let mut scheduler = Scheduler::new(&pages, &Stage::ALL);
        let mut busy = BTreeSet::new();
        let expected = [
            (0, Stage::Detection),
            (1, Stage::Detection),
            (0, Stage::Ocr),
            (1, Stage::Ocr),
            (0, Stage::Translation),
            (1, Stage::Translation),
            (0, Stage::Inpainting),
            (1, Stage::Inpainting),
        ];

        for (page, stage) in expected {
            assert_eq!(scheduler.start_next(&busy), Some((pages[page], stage)));
            busy.insert(stage);
            assert!(scheduler.start_next(&busy).is_none());
            let page_finished = scheduler.complete_stage(pages[page], stage);
            assert_eq!(page_finished, stage == Stage::Inpainting);
            busy.remove(&stage);
        }
        assert!(scheduler.start_next(&busy).is_none());
    }

    #[test]
    fn stage_batch_waits_for_every_page_before_advancing() {
        let pages = pages(4);
        let stages = [Stage::Detection, Stage::Ocr, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let mut busy = BTreeSet::new();

        for page in &pages {
            assert_eq!(scheduler.start_next(&busy), Some((*page, Stage::Detection)));
            busy.insert(Stage::Detection);
            assert!(scheduler.start_next(&busy).is_none());
            assert!(!scheduler.complete_stage(*page, Stage::Detection));
            busy.remove(&Stage::Detection);
        }

        assert_eq!(scheduler.start_next(&busy), Some((pages[0], Stage::Ocr)));
        busy.insert(Stage::Ocr);
        assert!(scheduler.start_next(&busy).is_none());
    }

    #[test]
    fn requested_stage_waits_for_its_requested_prerequisite() {
        let pages = pages(1);
        let stages = [Stage::Ocr, Stage::Translation, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let mut busy = BTreeSet::new();

        assert_eq!(scheduler.start_next(&busy), Some((pages[0], Stage::Ocr)));
        busy.insert(Stage::Ocr);
        assert!(scheduler.start_next(&busy).is_none());
        assert!(!scheduler.complete_stage(pages[0], Stage::Ocr));
        busy.remove(&Stage::Ocr);
        assert_eq!(
            scheduler.start_next(&busy),
            Some((pages[0], Stage::Translation))
        );
    }

    #[test]
    fn finish_stage_closes_pending_pages_before_the_next_stage() {
        let pages = pages(2);
        let stages = [Stage::Detection, Stage::Inpainting];
        let mut scheduler = Scheduler::new(&pages, &stages);
        let busy = BTreeSet::new();

        assert_eq!(scheduler.start_next(&busy), Some((pages[0], Stage::Detection)));
        scheduler.finish_stage(Stage::Detection);
        assert_eq!(
            scheduler.start_next(&busy),
            Some((pages[0], Stage::Inpainting))
        );
    }
}
