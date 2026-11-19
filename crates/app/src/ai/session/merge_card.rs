//! A design option merge reaches the live model only as a plan card: the worker compiles it on
//! the copy of the live model and the person's Apply writes it, as one undo step.

use super::*;
use crate::ai::opt_merge::{self, MergeCard, MergeOpts, MergeSource, Pick, Refusal};

impl Core {
    pub(super) fn open_merge(&mut self, mut card: MergeCard) {
        self.ack += 1;
        // the UI refuses this too; the worker never drops a plan the person has not decided on
        if !self.plan.items.is_empty() {
            self.send(FromWorker::Failed(AiError::Busy));
            self.publish();
            return;
        }
        opt_merge::shift_card(&mut card, self.plan.next_item, self.plan.next_slot);
        self.plan = std::mem::take(&mut card.plan);
        let res = self.rollback().and_then(|()| {
            self.ensure_baseline()?;
            plan::replay(
                &mut self.plan,
                &mut self.ed,
                &self.sets,
                &self.gens,
                &self.rules,
            )
            .map_err(AiError::from)
        });
        self.broken = res.is_err();
        match res {
            Ok(()) => {
                opt_merge::after_replay(&mut card, &self.plan);
                self.audit("option_merge", None, Some(opt_merge::summary(&card)), "ok");
            }
            Err(e) => {
                self.audit("option_merge", None, Some(opt_merge::summary(&card)), "failed");
                self.send(FromWorker::Failed(e));
                self.send(FromWorker::NeedSync);
            }
        }
        self.publish();
    }
}

impl UiSession {
    /// Puts a merge of `src` on the card. Nothing reaches the live model before Apply; an open
    /// plan is never replaced unasked.
    pub fn merge_option(
        &mut self,
        ed: &Editor,
        src: &MergeSource<'_>,
        pick: &Pick,
        opts: MergeOpts,
        note: Option<String>,
    ) -> Result<(), Refusal> {
        let open = self.plan.as_ref().is_some_and(|p| !p.items.is_empty());
        if self.busy || self.sync_out || open {
            return Err(Refusal::Busy);
        }
        let live = ed
            .model
            .ai_option_head()
            .map_err(|e| Refusal::Model(e.to_string()))?;
        let card = opt_merge::merge_card(src, pick, live, opts, (0, 0), note)?;
        self.merge_of = Some((card.option, card.note.clone()));
        self.acked_send(ToWorker::Merge(Box::new(card)));
        Ok(())
    }
}
