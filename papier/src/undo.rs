//! Op-based undo/redo, per page.
//!
//! The invariant that keeps memory small: an op owns stroke data ONLY
//! while that data is absent from the page (an applied erase holds the
//! erased strokes; an undone add holds the removed stroke). Everything
//! else is (Owner, id) references — stable because ids persist with the
//! page JSON.
//!
//! Stacks are keyed by the page's INK FILE PATH (stable across flips and
//! note insertion in both doc kinds) and survive page flips; the whole
//! map drops when the document closes. LIFO ordering makes "undo a move
//! after a later erase" correct by construction: the erase comes back
//! first, restoring the strokes the move references.

use crate::ink::{stroke_bbox, text_run_bbox, OwnedStroke, Owner, Page, PatchBody, Rect, Stroke};
use std::collections::HashMap;

/// Per-page op cap and a retained-point budget (an op's cost is the
/// points it OWNS; refs are free).
const UNDO_CAP: usize = 100;
const UNDO_PT_CAP: usize = 200_000; /* ~2.4 MB worst case */
const PAGES_LRU: usize = 8;

pub enum EditOp {
    /// A finished user stroke. `stroke` is Some only while UNDONE.
    AddStroke { id: u64, stroke: Option<Stroke> },
    /// A rubber batch or lasso delete. `strokes` is Some while APPLIED;
    /// `refs` always identifies the victims (for redo).
    EraseStrokes { refs: Vec<(Owner, u64)>, strokes: Option<Vec<OwnedStroke>> },
    /// A pixel-rubber contact: victims split into surviving fragments.
    /// `removed` (the originals) is Some while APPLIED; `added` (the
    /// fragments) is Some only while UNDONE. Refs always identify both.
    SplitStrokes {
        removed_refs: Vec<(Owner, u64)>,
        removed: Option<Vec<OwnedStroke>>,
        added_refs: Vec<(Owner, u64)>,
        added: Option<Vec<OwnedStroke>>,
    },
    /// A lasso move (M4).
    MoveStrokes { refs: Vec<(Owner, u64)>, dx: f32, dy: f32 },
    /// pi drew a patch. `body` (strokes + typeset texts) is Some only while UNDONE.
    AddPatch { id: u64, body: Option<PatchBody> },
    /// pi (or the rubber) erased a patch. `body` is Some while APPLIED.
    ErasePatch { id: u64, body: Option<PatchBody> },
}

impl EditOp {
    /// Build an erase op from the strokes `erase_at`/`remove_strokes_by_ids`
    /// just lifted out of the page.
    pub fn erased(lifted: Vec<OwnedStroke>) -> EditOp {
        let refs = lifted.iter().map(|o| (o.owner, o.stroke.id)).collect();
        EditOp::EraseStrokes { refs, strokes: Some(lifted) }
    }
}

fn op_owned_pts(op: &EditOp) -> usize {
    match op {
        EditOp::AddStroke { stroke, .. } => stroke.as_ref().map_or(0, |s| s.pts.len()),
        EditOp::EraseStrokes { strokes, .. } => {
            strokes.as_ref().map_or(0, |v| v.iter().map(|o| o.stroke.pts.len()).sum())
        }
        EditOp::SplitStrokes { removed, added, .. } => [removed, added]
            .iter()
            .filter_map(|o| o.as_ref())
            .flatten()
            .map(|o| o.stroke.pts.len())
            .sum(),
        EditOp::MoveStrokes { .. } => 0,
        EditOp::AddPatch { body, .. } | EditOp::ErasePatch { body, .. } => {
            body.as_ref().map_or(0, |(ss, _)| ss.iter().map(|s| s.pts.len()).sum())
        }
    }
}

fn body_bbox(body: &PatchBody) -> Option<Rect> {
    body.0
        .iter()
        .filter_map(stroke_bbox)
        .chain(body.1.iter().filter_map(text_run_bbox))
        .reduce(Rect::union)
}

#[derive(Default)]
pub struct UndoStack {
    undo: Vec<EditOp>,
    redo: Vec<EditOp>,
}

impl UndoStack {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Record a fresh USER op (it has already been applied to the page).
    /// A user edit invalidates the redo stack, as usual.
    pub fn push(&mut self, op: EditOp) {
        self.redo.clear();
        self.push_keep_redo(op);
    }

    /// Record an op WITHOUT discarding the redo stack. Used for pi's draws:
    /// the agent adding a margin note should not silently destroy the
    /// user's ability to redo their own just-undone stroke. Ops are
    /// id-keyed and independent, so a preserved redo re-applies cleanly on
    /// top of whatever pi added.
    pub fn push_keep_redo(&mut self, op: EditOp) {
        self.undo.push(op);
        while self.undo.len() > UNDO_CAP
            || (self.undo.iter().map(op_owned_pts).sum::<usize>() > UNDO_PT_CAP
                && self.undo.len() > 1)
        {
            self.undo.remove(0);
        }
    }

    /// Un-apply the newest op; returns the dirty rect to repaint.
    ///
    /// The op ALWAYS moves to the redo stack, even if its apply produced no
    /// visible change (a `?` inside the fallible block only yields a None
    /// dirty rect — it must never drop the op, or redo would silently lose
    /// it and its button would stay greyed).
    pub fn undo(&mut self, page: &mut Page) -> Option<Rect> {
        let mut op = self.undo.pop()?;
        let dirty = (|| -> Option<Rect> {
            match &mut op {
                EditOp::AddStroke { id, stroke } => {
                    let (s, b) = page.remove_stroke_by_id(*id)?;
                    *stroke = Some(s);
                    Some(b)
                }
                EditOp::EraseStrokes { strokes, .. } => page.insert_owned(strokes.take()?),
                EditOp::SplitStrokes { removed, added_refs, added, .. } => {
                    /* fragments out, originals back */
                    let (lifted, b1) = page.remove_strokes_by_ids(added_refs);
                    *added = Some(lifted);
                    let b2 = page.insert_owned(removed.take()?);
                    match (b1, b2) {
                        (Some(a), Some(b)) => Some(a.union(b)),
                        (a, b) => a.or(b),
                    }
                }
                EditOp::MoveStrokes { refs, dx, dy } => page.translate_strokes(refs, -*dx, -*dy),
                EditOp::AddPatch { id, body } => {
                    let (content, b) = page.take_patch(*id)?;
                    *body = Some(content);
                    Some(b)
                }
                EditOp::ErasePatch { id, body } => {
                    let content = body.take()?;
                    let b = body_bbox(&content);
                    page.add_patch_with_id(*id, content);
                    b
                }
            }
        })();
        self.redo.push(op);
        dirty
    }

    /// Re-apply the newest undone op; returns the dirty rect. Symmetric to
    /// `undo`: the op always moves back to the undo stack.
    pub fn redo(&mut self, page: &mut Page) -> Option<Rect> {
        let mut op = self.redo.pop()?;
        let dirty = (|| -> Option<Rect> {
            match &mut op {
                EditOp::AddStroke { id, stroke } => {
                    let s = stroke.take()?;
                    let b = stroke_bbox(&s);
                    *id = s.id;
                    page.next_stroke = page.next_stroke.max(s.id + 1);
                    page.strokes.push(s);
                    page.dirty = true;
                    b
                }
                EditOp::EraseStrokes { refs, strokes } => {
                    let (lifted, b) = page.remove_strokes_by_ids(refs);
                    *strokes = Some(lifted);
                    b
                }
                EditOp::SplitStrokes { removed_refs, removed, added, .. } => {
                    /* originals out, fragments back */
                    let (lifted, b1) = page.remove_strokes_by_ids(removed_refs);
                    *removed = Some(lifted);
                    let b2 = page.insert_owned(added.take()?);
                    match (b1, b2) {
                        (Some(a), Some(b)) => Some(a.union(b)),
                        (a, b) => a.or(b),
                    }
                }
                EditOp::MoveStrokes { refs, dx, dy } => page.translate_strokes(refs, *dx, *dy),
                EditOp::AddPatch { id, body } => {
                    let content = body.take()?;
                    let b = body_bbox(&content);
                    page.add_patch_with_id(*id, content);
                    b
                }
                EditOp::ErasePatch { id, body } => {
                    let (content, b) = page.take_patch(*id)?;
                    *body = Some(content);
                    Some(b)
                }
            }
        })();
        self.undo.push(op);
        dirty
    }
}

/// Stacks per ink-file path, LRU-capped.
#[derive(Default)]
pub struct PerPage {
    map: HashMap<String, UndoStack>,
    order: Vec<String>, /* most recent last */
}

impl PerPage {
    pub fn stack(&mut self, key: &str) -> &mut UndoStack {
        if !self.map.contains_key(key) {
            self.map.insert(key.to_string(), UndoStack::default());
        }
        self.order.retain(|k| k != key);
        self.order.push(key.to_string());
        while self.order.len() > PAGES_LRU {
            let old = self.order.remove(0);
            self.map.remove(&old);
        }
        self.map.get_mut(key).unwrap()
    }

    pub fn peek(&self, key: &str) -> Option<&UndoStack> {
        self.map.get(key)
    }

    /// Drop a page's history (e.g. pi mutated a NON-current page on disk —
    /// our ids for it are stale).
    pub fn drop_page(&mut self, key: &str) {
        self.map.remove(key);
        self.order.retain(|k| k != key);
    }
}
