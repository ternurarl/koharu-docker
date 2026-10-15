//! Out-of-order commit coverage for overlapping pipeline stages.
//!
//! The pipeline is moving from a per-stage barrier to concurrent Ocr,
//! Translation, and Inpainting work, so stage patches for one page will arrive
//! and commit in whatever order the jobs finish. These tests rebuild the exact
//! edit shapes `stages::ocr`, `stages::translation`, and `stages::inpainting`
//! produce — the same producer ids, the same observation sets, the same
//! component writes — and check that `Patch::rebase_on` / `Patch::validate_on`
//! accept every order a concurrent run can produce and still reject the
//! rewrites they must reject.
//!
//! What is modelled literally and what is not:
//!
//! * `stages::inpainting::prepare` only reads the scene and the image cache, so
//!   it contributes no observations. The manual-mask variant differs from the
//!   automatic one only in `Snapshot::edit` versus `Snapshot::edit_as`, which
//!   changes authorship stamping, not the observation set; both are covered.
//! * Model inference is not run. The tests write the text and the cleanup
//!   raster the processors would have produced after inference. What is under
//!   test is the commit protocol, not the models.
//! * The fixture's source text is created by an Ocr-shaped patch, not authored
//!   by the user, because `Edit` refuses to let a pipeline edit overwrite a
//!   user-owned component — which is also why `stages::ocr` skips targets whose
//!   source text has `Origin::User`.
//! * `WorkerCommitter` is private to `crate::worker`, so the subprocess path is
//!   modelled by reproducing its stamping rule (`Patch::to_transfer`, overwrite
//!   `project` and `base` with the parent's document and the revision the parent
//!   is predicted to be at, `Patch::from_transfer` on the parent).

use std::collections::BTreeMap;

use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Authored, EntityId, Error, Generation, Geometry,
    LanguageTag, OcrAnalysis, Origin, PageDraft, Patch, ProducerId, RasterLayer, RasterLayerKind,
    RecognizedFrom, Region, Revision, Session, Snapshot, SourceText, TextDirection, TextLayout,
    TextLayoutKind, TextRegion, Translation,
};

/// `stages::ocr::PRODUCER`.
const OCR_PRODUCER: &str = "dev.koharu.pipeline.ocr";
/// `stages::translation::PRODUCER`.
const TRANSLATION_PRODUCER: &str = "dev.koharu.pipeline.translation";
/// `stages::inpainting::PRODUCER`.
const INPAINTING_PRODUCER: &str = "dev.koharu.pipeline.inpainting";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct PageFixture {
    page: EntityId,
    region: EntityId,
    content: EntityId,
}

/// The page shape detection leaves behind: a source asset, one text region with
/// geometry, one text content entity, one text layer presenting it, and the
/// `RecognizedFrom` relation Ocr walks to find its targets. No source text yet —
/// that is Ocr's write.
async fn add_page_fixture(session: &mut Session, label: &str) -> PageFixture {
    let mut ids = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let page = edit.add_page(PageDraft::new(label, 100.0, 100.0), At::End)?;
            edit.set_asset(page, &AssetRole::new("source")?, asset(b"source"))?;
            let region = edit.add_analysis_region::<TextRegion>(
                page,
                At::End,
                &Geometry::rectangle(10.0, 10.0, 40.0, 20.0),
                None,
            )?;
            let content = edit.add_text_content(page, At::End)?;
            edit.add_text_layer(
                page,
                At::End,
                content,
                &TextLayout {
                    origin: Origin::User,
                    kind: TextLayoutKind::Paragraph,
                },
            )?;
            edit.relate::<RecognizedFrom>(content, region)?;
            ids = Some(PageFixture {
                page,
                region,
                content,
            });
            Ok(())
        })
        .unwrap();
    session.commit(patch).await.unwrap();
    ids.expect("page IDs are assigned by the edit")
}

/// A session holding one detected page, the snapshot before Ocr wrote its
/// source text, and the session's snapshot after that first Ocr run. The seed
/// is committed for real, because the subprocess tests need the session's
/// revision sequence to match the snapshot chain.
async fn fixture_session() -> (Session, PageFixture, Snapshot, Snapshot) {
    let mut session = Session::memory().await.unwrap();
    let page = add_page_fixture(&mut session, "page").await;
    let detected = session.snapshot();
    let seed = ocr_patch(&detected, page, "before");
    session
        .commit(seed.rebase_on(&detected).unwrap())
        .await
        .unwrap();
    let seeded = session.snapshot();
    (session, page, detected, seeded)
}

fn asset(bytes: &'static [u8]) -> AssetInput {
    AssetInput::new(
        bytes,
        "image/png",
        AssetMetadata {
            width: Some(1),
            height: Some(1),
            attributes: BTreeMap::new(),
        },
    )
}

fn generation(producer: &str, model: &str) -> Generation {
    let mut generation = Generation::new(ProducerId::new(producer).unwrap());
    generation.model = Some(model.to_owned());
    generation
}

/// Mirrors `stages::inpainting::prepare`'s cleanup lookup.
fn cleanup_layer(scene: &Snapshot, page: EntityId) -> Option<EntityId> {
    scene.children(page).unwrap().find(|entity| {
        scene
            .component::<RasterLayer>(*entity)
            .unwrap()
            .is_some_and(|layer| layer.kind == RasterLayerKind::Cleanup)
    })
}

fn source_text(scene: &Snapshot, page: PageFixture) -> Option<String> {
    scene
        .component::<SourceText>(page.content)
        .unwrap()
        .map(|value| value.text.value)
}

fn translation_text(scene: &Snapshot, page: PageFixture) -> Option<String> {
    scene
        .component::<Translation>(page.content)
        .unwrap()
        .map(|value| value.text.value)
}

// ---------------------------------------------------------------------------
// Stage patch shapes
// ---------------------------------------------------------------------------

/// Mirrors the tail of `stages::ocr::Model::run`.
fn ocr_patch(base: &Snapshot, page: PageFixture, text: &str) -> Patch {
    let generated = generation(OCR_PRODUCER, "paddleocr-vl-1.6");
    let mut edit = base.edit_as(generated.clone());
    edit.observe_assets(page.page).unwrap();
    edit.observe::<Region>(page.region).unwrap();
    edit.observe::<Geometry>(page.region).unwrap();
    edit.observe::<SourceText>(page.content).unwrap();
    edit
        .set(
            page.content,
            &SourceText {
                text: Authored::generated(text.to_owned(), generated.clone()),
                language: LanguageTag::new("ja-JP").ok(),
            },
        )
        .unwrap();
    edit
        .set(
            page.region,
            &OcrAnalysis {
                origin: Origin::Generated(generated),
                direction: TextDirection::Horizontal,
                confidence: None,
                line_boundaries: Vec::new(),
            },
        )
        .unwrap();
    edit.finish().unwrap()
}

/// Mirrors the tail of `stages::translation::Processor::process`.
///
/// The translated string records the source text the request was built from, so
/// a test can tell a translation of the pre-Ocr text from one of the post-Ocr
/// text without inspecting patch internals.
fn translation_patch(base: &Snapshot, page: PageFixture) -> Patch {
    let generated = generation(TRANSLATION_PRODUCER, "local");
    let source = source_text(base, page).expect("translation reads an existing source text");
    let mut edit = base.edit_as(generated.clone());
    edit.observe::<SourceText>(page.content).unwrap();
    edit.observe::<Translation>(page.content).unwrap();
    edit
        .set(
            page.content,
            &Translation {
                text: Authored::generated(format!("EN<{source}>"), generated),
                language: LanguageTag::new("en-US").ok(),
            },
        )
        .unwrap();
    edit.finish().unwrap()
}

/// Mirrors the tail of `stages::inpainting::Model::run`.
///
/// `manual_mask` selects the `Snapshot::edit` branch the transient-mask request
/// takes; every other branch of the edit is identical.
fn inpainting_patch(base: &Snapshot, page: PageFixture, manual_mask: bool) -> Patch {
    let generated = generation(INPAINTING_PRODUCER, "lama");
    let mut edit = if manual_mask {
        base.edit()
    } else {
        base.edit_as(generated)
    };
    edit.observe_assets(page.page).unwrap();
    let existing = cleanup_layer(base, page.page);
    if let Some(entity) = existing {
        edit.observe::<RasterLayer>(entity).unwrap();
        edit.observe_assets(entity).unwrap();
    }
    let cleanup = match existing {
        Some(entity) => entity,
        None => {
            let entity = edit.add_entity(page.page, At::Start).unwrap();
            edit.set(
                entity,
                &RasterLayer {
                    origin: Origin::User,
                    name: "Cleanup".to_owned(),
                    kind: RasterLayerKind::Cleanup,
                },
            )
            .unwrap();
            entity
        }
    };
    edit.set_asset(
        cleanup,
        &AssetRole::new("source").unwrap(),
        asset(b"cleanup"),
    )
    .unwrap();
    edit.finish().unwrap()
}

// ---------------------------------------------------------------------------
// Commit protocol
// ---------------------------------------------------------------------------

/// Mirrors `Execution::commit_patch`: rebase onto the latest scene, then
/// validate the rebased patch against that same scene.
fn commit_sequence(base: &Snapshot, patches: &[&Patch]) -> Result<Snapshot, Error> {
    let mut current = base.clone();
    for patch in patches {
        let rebased = patch.rebase_on(&current)?;
        rebased.validate_on(&current)?;
        current = current.preview([&rebased])?;
    }
    Ok(current)
}

/// Mirrors `Execution::commit_patch` followed by `WorkerCommitter::commit` and
/// the parent's `Patch::from_transfer` rebuild, for one forwarded stage output.
///
/// `WorkerCommitter` is private to `crate::worker`, so its three-line stamping
/// rule is reproduced here: `Patch::to_transfer`, then overwrite `project` and
/// `base` with the parent's document and the revision the parent is predicted
/// to be at. Production commits the rebased patch on the child session and the
/// transfer-rebuilt patch on the parent session; this model keeps one session,
/// which is equivalent as long as nothing else writes to it, exactly as
/// `worker::execute` assumes.
async fn forward_like_worker(
    session: &mut Session,
    predicted_base: Revision,
    stage: &str,
    page: EntityId,
    patch: &Patch,
) -> Result<Revision, Error> {
    let current = session.snapshot();
    let rebased = patch.rebase_on(&current)?;
    rebased.validate_on(&current)?;
    let mut transfer = rebased.to_transfer(stage, page);
    transfer.project = current.project_id().document();
    transfer.base = predicted_base;
    let rebuilt = Patch::from_transfer(&current, transfer)?;
    session.commit(rebuilt).await?;
    predicted_base
        .next()
        .ok_or_else(|| Error::Invalid("revision overflow".to_owned()))
}

// ---------------------------------------------------------------------------
// 1. Ocr || Inpainting on the same page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ocr_and_inpainting_for_one_page_rebase_in_either_order() {
    for manual_mask in [false, true] {
        // Once where Ocr writes the source text for the first time, once where
        // it overwrites the source text an earlier Ocr run produced.
        let (_session, page, detected, seeded) = fixture_session().await;
        for base in [&detected, &seeded] {
            let ocr = ocr_patch(base, page, "recognized");
            let inpaint = inpainting_patch(base, page, manual_mask);

            let ocr_first = commit_sequence(base, &[&ocr, &inpaint]).unwrap();
            let inpaint_first = commit_sequence(base, &[&inpaint, &ocr]).unwrap();

            for final_state in [&ocr_first, &inpaint_first] {
                assert_eq!(source_text(final_state, page).as_deref(), Some("recognized"));
                assert_eq!(
                    final_state
                        .component::<OcrAnalysis>(page.region)
                        .unwrap()
                        .map(|value| value.direction),
                    Some(TextDirection::Horizontal)
                );
                assert!(cleanup_layer(final_state, page.page).is_some());
            }
        }
    }
}

/// A page that already carries a cleanup layer takes the other Inpainting
/// branch: it observes the layer's `RasterLayer` and `Assets` instead of
/// inserting an entity, and rewrites the asset in place.
#[tokio::test]
async fn ocr_and_inpainting_over_an_existing_cleanup_layer_rebase_in_either_order() {
    let (_session, page, _detected, seeded) = fixture_session().await;
    let first = inpainting_patch(&seeded, page, false);
    let established = commit_sequence(&seeded, &[&first]).unwrap();
    let cleanup = cleanup_layer(&established, page.page).unwrap();

    let ocr = ocr_patch(&established, page, "recognized");
    let refresh = inpainting_patch(&established, page, false);

    let ocr_first = commit_sequence(&established, &[&ocr, &refresh]).unwrap();
    let refresh_first = commit_sequence(&established, &[&refresh, &ocr]).unwrap();

    for final_state in [&ocr_first, &refresh_first] {
        assert_eq!(source_text(final_state, page).as_deref(), Some("recognized"));
        assert_eq!(cleanup_layer(final_state, page.page), Some(cleanup));
    }
}

// ---------------------------------------------------------------------------
// 2. Ocr || Translation || Inpainting on the same page
// ---------------------------------------------------------------------------

/// `Scheduler::prerequisite` makes `Translation` wait for `Ocr` on the same
/// page, so a concurrent run builds the translation patch against a scene that
/// already contains Ocr's writes. Those are the three commit orders the new
/// scheduler can produce.
#[tokio::test]
async fn ocr_translation_and_inpainting_interleave_in_every_producible_order() {
    let (_session, page, _detected, seeded) = fixture_session().await;

    let ocr = ocr_patch(&seeded, page, "recognized");
    let inpaint = inpainting_patch(&seeded, page, false);
    let after_ocr = commit_sequence(&seeded, &[&ocr]).unwrap();
    let translation = translation_patch(&after_ocr, page);

    let orders: [&[&Patch]; 3] = [
        &[&ocr, &translation, &inpaint],
        &[&ocr, &inpaint, &translation],
        &[&inpaint, &ocr, &translation],
    ];
    for order in orders {
        let final_state = commit_sequence(&seeded, order).unwrap();
        assert_eq!(source_text(&final_state, page).as_deref(), Some("recognized"));
        assert_eq!(
            translation_text(&final_state, page).as_deref(),
            Some("EN<recognized>")
        );
        assert!(cleanup_layer(&final_state, page.page).is_some());
    }
}

/// The orders a run must never produce. If the scheduler ever lets Translation
/// start before Ocr for the same page, the patch layer either rejects the
/// result or silently keeps a translation of the wrong source text.
#[tokio::test]
async fn translation_before_ocr_is_rejected_or_stale_and_needs_the_scheduler() {
    let (_session, page, _detected, seeded) = fixture_session().await;

    let ocr = ocr_patch(&seeded, page, "recognized");
    // The patch a translation job would build if it raced ahead of Ocr.
    let early = translation_patch(&seeded, page);
    let after_ocr = commit_sequence(&seeded, &[&ocr]).unwrap();
    // The patch a translation job builds once Ocr has finished, which is what
    // `Scheduler::prerequisite` guarantees.
    let late = translation_patch(&after_ocr, page);

    // Ocr committing first invalidates the early translation's observations.
    let error = commit_sequence(&seeded, &[&ocr, &early]).unwrap_err();
    assert!(
        matches!(&error, Error::PatchConflict(message) if message.contains("observed component")),
        "unexpected rejection: {error}"
    );

    // The late translation cannot be committed before Ocr either: its
    // observations already record Ocr's source text.
    let error = commit_sequence(&seeded, &[&late, &ocr]).unwrap_err();
    assert!(
        matches!(&error, Error::PatchConflict(message) if message.contains("observed component")),
        "unexpected rejection: {error}"
    );

    // The early translation committed first is accepted, and leaves a
    // translation of the pre-Ocr source text next to Ocr's new source text.
    // Nothing in the patch layer notices; `Scheduler::prerequisite` is the only
    // guard, and the concurrency change must not weaken it.
    let stale = commit_sequence(&seeded, &[&early, &ocr]).unwrap();
    assert_eq!(source_text(&stale, page).as_deref(), Some("recognized"));
    assert_eq!(
        translation_text(&stale, page).as_deref(),
        Some("EN<before>")
    );
}

// ---------------------------------------------------------------------------
// 3. Cross-page independence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patches_for_different_pages_commute() {
    let mut session = Session::memory().await.unwrap();
    let first = add_page_fixture(&mut session, "first").await;
    let second = add_page_fixture(&mut session, "second").await;
    let detected = session.snapshot();
    let seeded = commit_sequence(
        &detected,
        &[
            &ocr_patch(&detected, first, "before"),
            &ocr_patch(&detected, second, "before"),
        ],
    )
    .unwrap();

    let pairs: [(Patch, Patch); 4] = [
        (
            ocr_patch(&seeded, first, "recognized"),
            inpainting_patch(&seeded, second, false),
        ),
        (
            ocr_patch(&seeded, second, "recognized"),
            inpainting_patch(&seeded, first, false),
        ),
        (
            translation_patch(&seeded, first),
            inpainting_patch(&seeded, second, false),
        ),
        (
            ocr_patch(&seeded, first, "recognized"),
            ocr_patch(&seeded, second, "recognized"),
        ),
    ];
    for (left, right) in pairs {
        let left_first = commit_sequence(&seeded, &[&left, &right]).unwrap();
        let right_first = commit_sequence(&seeded, &[&right, &left]).unwrap();
        // Commuting means both orders land on the same scene.
        for page in [first, second] {
            assert_eq!(
                source_text(&left_first, page),
                source_text(&right_first, page)
            );
            assert_eq!(
                translation_text(&left_first, page),
                translation_text(&right_first, page)
            );
            assert_eq!(
                cleanup_layer(&left_first, page.page),
                cleanup_layer(&right_first, page.page)
            );
        }
    }

    // A representative pairing, asserted against the values the patches wrote.
    let ocr_first = ocr_patch(&seeded, first, "recognized");
    let inpaint_second = inpainting_patch(&seeded, second, false);
    let final_state = commit_sequence(&seeded, &[&ocr_first, &inpaint_second]).unwrap();
    assert_eq!(source_text(&final_state, first).as_deref(), Some("recognized"));
    assert_eq!(source_text(&final_state, second).as_deref(), Some("before"));
    assert!(cleanup_layer(&final_state, first.page).is_none());
    assert!(cleanup_layer(&final_state, second.page).is_some());

    // A translation for one page keeps its own source text when the other
    // page's patches land around it in either order.
    let translation = translation_patch(&seeded, first);
    let ocr_second = ocr_patch(&seeded, second, "recognized");
    let final_state =
        commit_sequence(&seeded, &[&inpaint_second, &translation, &ocr_second]).unwrap();
    assert_eq!(
        translation_text(&final_state, first).as_deref(),
        Some("EN<before>")
    );
    assert_eq!(source_text(&final_state, second).as_deref(), Some("recognized"));
}

// ---------------------------------------------------------------------------
// 4. Concurrent Inpainting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_inpainting_patches_for_different_pages_commute() {
    let mut session = Session::memory().await.unwrap();
    let first = add_page_fixture(&mut session, "first").await;
    let second = add_page_fixture(&mut session, "second").await;
    let base = session.snapshot();

    let left = inpainting_patch(&base, first, false);
    let right = inpainting_patch(&base, second, false);

    let left_first = commit_sequence(&base, &[&left, &right]).unwrap();
    let right_first = commit_sequence(&base, &[&right, &left]).unwrap();
    for final_state in [&left_first, &right_first] {
        assert!(cleanup_layer(final_state, first.page).is_some());
        assert!(cleanup_layer(final_state, second.page).is_some());
    }
}

/// Two Inpainting patches for the *same* page both observe the page's children
/// before inserting a cleanup entity, so the second one must be rejected. This
/// is why same-page Inpainting concurrency stays out of scope.
#[tokio::test]
async fn two_inpainting_patches_for_the_same_page_conflict() {
    let (_session, page, detected, _seeded) = fixture_session().await;

    let first = inpainting_patch(&detected, page, false);
    let second = inpainting_patch(&detected, page, false);

    let error = commit_sequence(&detected, &[&first, &second]).unwrap_err();
    assert!(
        matches!(&error, Error::PatchConflict(message) if message.contains("children of entity")),
        "unexpected rejection: {error}"
    );
}

// ---------------------------------------------------------------------------
// 5. What must keep failing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_text_rewrites_are_still_rejected() {
    let (_session, page, _detected, seeded) = fixture_session().await;
    let translation = translation_patch(&seeded, page);

    // The guard the existing `stages::tests` case covers: an intervening source
    // rewrite invalidates the translation's observations.
    let rewritten = seeded
        .patch(|edit| {
            edit.set(
                page.content,
                &SourceText {
                    text: Authored::user("changed".to_owned()),
                    language: None,
                },
            )
        })
        .unwrap();
    let rewritten = seeded.preview([&rewritten]).unwrap();
    let error = translation.rebase_on(&rewritten).unwrap_err();
    assert!(
        matches!(&error, Error::PatchConflict(message) if message.contains("observed component")),
        "unexpected rejection: {error}"
    );

    // The concurrency-relevant form of the same guard: Ocr's rewrite of the
    // source text invalidates a translation patch built before it ran.
    let ocr = ocr_patch(&seeded, page, "recognized");
    let after_ocr = commit_sequence(&seeded, &[&ocr]).unwrap();
    let error = translation.validate_on(&after_ocr).unwrap_err();
    assert!(
        matches!(&error, Error::PatchConflict(message) if message.contains("observed component")),
        "unexpected rejection: {error}"
    );

    // Inpainting must not weaken either guard: it commutes with both.
    let inpaint = inpainting_patch(&seeded, page, false);
    let after_inpaint = commit_sequence(&seeded, &[&inpaint]).unwrap();
    translation.validate_on(&after_inpaint).unwrap();
}

// ---------------------------------------------------------------------------
// 6. The subprocess path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transfer_forwards_out_of_order_stage_patches_in_lockstep() {
    for order in [[0_usize, 1], [1, 0]] {
        let (mut session, page, _detected, seeded) = fixture_session().await;
        let patches = [
            ("inpainting", inpainting_patch(&seeded, page, false)),
            ("ocr", ocr_patch(&seeded, page, "recognized")),
        ];
        // `WorkerCommitter` predicts the parent's next revision as a counter
        // advanced by one per forwarded commit.
        let mut predicted = seeded.revision();
        for index in order {
            let (stage, patch) = &patches[index];
            predicted = forward_like_worker(&mut session, predicted, stage, page.page, patch)
                .await
                .unwrap_or_else(|error| panic!("{stage} in order {order:?}: {error}"));
        }

        let final_state = session.snapshot();
        assert_eq!(source_text(&final_state, page).as_deref(), Some("recognized"));
        assert!(cleanup_layer(&final_state, page.page).is_some());
    }
}

#[tokio::test]
async fn transfer_rejects_a_base_stamp_out_of_lockstep() {
    let (mut session, page, _detected, seeded) = fixture_session().await;
    let ocr = ocr_patch(&seeded, page, "recognized");
    let inpaint = inpainting_patch(&seeded, page, false);
    let start = seeded.revision();

    // The first forwarded commit moves the parent on by one revision.
    forward_like_worker(&mut session, start, "ocr", page.page, &ocr)
        .await
        .unwrap();

    // Reusing the starting prediction for the next forwarded patch is the
    // desync landmine 18 warns about. The parent is one revision ahead, so the
    // transfer has to be refused.
    let error = forward_like_worker(&mut session, start, "inpainting", page.page, &inpaint)
        .await
        .expect_err("a base stamp out of lockstep must not be accepted");
    assert!(
        matches!(&error, Error::Storage(_)),
        "unexpected rejection: {error}"
    );
}

#[tokio::test]
async fn transfer_rejects_an_observation_invalidated_by_an_earlier_commit() {
    let (mut session, page, _detected, seeded) = fixture_session().await;
    let ocr = ocr_patch(&seeded, page, "recognized");
    let early_translation = translation_patch(&seeded, page);

    let predicted_base = session.snapshot().revision();
    let predicted = forward_like_worker(&mut session, predicted_base, "ocr", page.page, &ocr)
        .await
        .unwrap();

    // The parent rebuilds each transfer against its running scene, so an
    // observation invalidated by an earlier forwarded commit is caught here as
    // well as by `Patch::rebase_on` on the in-process path.
    let current = session.snapshot();
    let mut transfer = early_translation.to_transfer("translation", page.page);
    transfer.project = current.project_id().document();
    transfer.base = predicted;
    let error = Patch::from_transfer(&current, transfer)
        .expect_err("a stale translation transfer must not be accepted");
    assert!(
        matches!(&error, Error::PatchConflict(message) if message.contains("observed component")),
        "unexpected rejection: {error}"
    );
}
