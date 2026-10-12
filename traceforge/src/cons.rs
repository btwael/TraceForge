use crate::event::Event;
use crate::event_label::{AsEventLabel, Inbox, LabelEnum, RecvMsg, SendMsg};
use crate::exec_graph::ExecutionGraph;
use crate::future::PollerMsg;
use crate::loc::CommunicationModel;
use crate::loc::WakeMsg;
use crate::revisit::{Revisit, RevisitPlacement};
use crate::vector_clock::VectorClock;
use log::debug;

// A generic consistency which will, eventually, support arbitrary
// communication models, depending on the channel.

// The intended semantics of consistent(G) is
// 1. porf-acyclic(G)
// 2. for each communication model M, the restriction of G
// to events of models not stronger than M is consistent under M
// 3. receive events of monitors act as if they are CausalOrder

pub(crate) struct Consistency {}

impl Consistency {
    // Checks if there is a TotalOrder relation between the two sends slab1 and slab2
    fn send_before(
        &self,
        g: &ExecutionGraph,
        sends: &[&SendMsg],
        slab1: Event,
        slab2: Event,
    ) -> bool {
        // Apart from slab1, also do not query send_before(slab2, slab2)
        Self::aux_send_before(g, sends, slab1, slab2, &mut vec![slab1, slab2])
    }

    // send_before = (porf U induced_send_before)^+
    fn aux_send_before(
        g: &ExecutionGraph,
        sends: &[&SendMsg],
        slab1: Event,
        // Every recursive call uses the same slab2
        slab2: Event,
        // Events s that we shouldn't query for send_before(s, slab2),
        // either because we are already (nested) in the process of answering the query
        // or because we already know that the query returns false
        seen: &mut Vec<Event>,
    ) -> bool {
        // porf <= send_before
        if g.send_label(slab2).unwrap().porf().contains(slab1) {
            return true;
        }

        // Transitivity: [slab1];send_before;[slab];send_before <= send_before
        for &slab in sends {
            // Only use TotalOrder sends as transitive steps
            if slab.comm() != CommunicationModel::TotalOrder {
                continue;
            }

            // Avoid recursing on send_before(s, s2), for some s that has already been tried
            if seen.contains(&slab.pos()) {
                continue;
            }

            // Check if [slab1];porf;[slab];send_before;[slab2]
            if slab.porf().contains(slab1) {
                seen.push(slab.pos());
                if Self::aux_send_before(g, sends, slab.pos(), slab2, seen) {
                    return true;
                }
            }

            // Lost sends cannot be pending mailbox competitors.
            if slab.is_dropped() {
                continue;
            }

            // Check if [slab1];induced_send_before;[slab];send_before;[slab2]
            // where (s1, s2) in induced_send_before iff s1 is read by a r1 that also matches s2 and
            // s2 is not read by the same or an earlier receive r2.
            // Since there are no concurrent receives, "later" means porf.

            // slab1 is read
            let rlab1 = match g.send_label(slab1).unwrap().reader() {
                Some(rlab1) => rlab1,
                None => continue,
            };

            // by a receive rlab1 that could also read slab
            if !Self::reader_matches(g, rlab1, slab) {
                continue;
            }

            // slab is unread, or read by a strictly later receive (not the same inbox).
            if slab
                .reader()
                .is_none_or(|rlab| rlab != rlab1 && g.in_porf(rlab1, rlab))
            {
                seen.push(slab.pos());
                if Self::aux_send_before(g, sends, slab.pos(), slab2, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// Returns the subset of the sends s.t. they can be read from rlab after (possibly) restricting the graph to the view.
    /// The view implicitly excludes one event: View = (VectorClock, excluded Event)
    /// Lack of view implies we consider the whole graph.
    fn filter_available_sends_in_view<'a>(
        g: &'a ExecutionGraph,
        rlab: &'a RecvMsg,
        sends: impl Iterator<Item = &'a SendMsg>,
        view: Option<(&'a VectorClock, Option<Event>)>,
        check_concurrent: bool,
    ) -> impl Iterator<Item = &'a SendMsg> {
        // println!("====== Started filter sends call");
        let rpos = rlab.pos();
        sends.filter(move |&slab| {
            let spos = slab.pos();

            // exclude one event
            if view.is_some_and(|(_, excl)| excl.is_some_and(|ev| ev == spos)) {
                return false;
            }

            // send must be in the view, if it exists
            if view.is_some_and(|view| !view.0.contains(slab.pos())) {
                return false;
            }

            // *Assumption*: if this send message is monitored by the receive's thread,
            // it cannot be that it is a send message towards the monitor itself.
            if slab.is_monitored_from(&rpos.thread) {
                !slab.monitor_readers().iter().any(|&reader| {
                    // As long as it is not monitor-read by an same-thread (same-monitor)
                    // event that would remain in the view, the send can be monitor-read.
                    //
                    // Exclude the receive itself
                    reader != rpos
                        && reader.thread == rpos.thread
                        && view.is_none_or(|view| view.0.contains(reader))
                })
            } else {
                // there is no reader, or the reader is *not* in the view (we exclude the receive itself)
                debug!("Started looking at send {}", slab);
                match slab.reader() {
                    None => true,
                    Some(reader) => {
                        debug!("He is received at {:?}", reader);
                        // Check for concurrent receives.
                        // We shouldn't include the receive's rf, i.e. use cached_porf.

                        // N.B. it should suffice to check only when we add rlab
                        // (it's the last event in its thread).
                        // Otherwise, we should also check whether
                        // rlab is before reader OR reader is bofore rlab
                        if check_concurrent && !rlab.cached_porf().contains(reader) {
                            println!("{}", g);
                            panic!(
                                "Detected concurrent receives: {} and {}",
                                reader,
                                rlab.pos()
                            );
                        }
                        reader == rpos
                            || view.is_some_and(|view| !view.0.contains(reader))
                            // A send is available if its reader was part of an async
                            // receive that was subsequently cancelled.
                            // Exclude internal PollerMsg sends.
                            || slab.val.as_any_ref().downcast_ref::<PollerMsg>().is_none()
                                &&
                               slab.val.as_any_ref().downcast_ref::<WakeMsg>().is_none()
                                && {
                                debug!("Inside cancel looking looking at thread with labels {:?}", g.get_thr(&reader.thread).labels);
                                let cancel_available = g.get_thr(&reader.thread).labels[(reader.index as usize + 1)..]
                                    .iter()
                                    .any(|lab| {
                                        if let LabelEnum::RecvMsg(recv) = lab {
                                            debug!("Searching for cancel: looking at receive from {:?}", recv.rf());
                                            recv.rf().is_some_and(|rf| {
                                                if let LabelEnum::SendMsg(send) = g.label(rf) {
                                                    debug!("And this send contains the message {:?}", send.val);
                                                    send.val.as_any_ref().downcast_ref::<PollerMsg>()
                                                        .is_some_and(|msg| matches!(msg, PollerMsg::Cancel))
                                                } else {
                                                    false
                                                }
                                            }) && view.is_none_or(|view| view.0.contains(lab.pos()))
                                        } else {
                                            false
                                        }
                                    });
                                if cancel_available {
                                    debug!("[cancel_path] send {} (reader={}) made available via cancel path", spos, reader);
                                    slab.push_cancelled_recv_reader(reader);
                                }
                                cancel_available
                            }
                    }
                }
            }
        })
    }

    fn filter_available_sends_in_view_for_inbox<'a>(
        g: &'a ExecutionGraph,
        ilab: &'a Inbox,
        sends: impl Iterator<Item = &'a SendMsg>,
        view: Option<(&'a VectorClock, Option<Event>)>,
        check_concurrent: bool,
    ) -> impl Iterator<Item = &'a SendMsg> {
        let rpos = ilab.pos();
        sends.filter(move |&slab| {
            let spos = slab.pos();

            // Revisit view can explicitly exclude one send.
            if view.is_some_and(|(_, excl)| excl.is_some_and(|ev| ev == spos)) {
                return false;
            }

            // Keep only sends present in the chosen prefix view.
            if view.is_some_and(|view| !view.0.contains(slab.pos())) {
                return false;
            }

            match slab.reader() {
                None => true,
                Some(reader) => {
                    // Same concurrency sanity check as plain receives.
                    if check_concurrent && !ilab.cached_porf().contains(reader) {
                        println!("{}", g);
                        panic!(
                            "Detected concurrent receives: {} and {}",
                            reader,
                            ilab.pos()
                        );
                    }
                    // Keep send if it is still unread in the view, or already read by this inbox.
                    reader == rpos || view.is_some_and(|view| !view.0.contains(reader))
                }
            }
        })
    }

    /// Returns whether the send has no sb-predecessor (porf-predecessors if flag is set) among the rest sends
    fn is_sb_miminal(send: &SendMsg, sends: &[&SendMsg], porf_override: bool) -> bool {
        let view = if porf_override {
            send.porf()
        } else {
            send.sb()
        };
        !sends.iter().any(|&e| view.contains(e.pos()))
    }

    /// Keeps the sb-minimals (porf-minimals is flag is set) among the (*stamp-ordered*) sends
    fn retain_sb_minimals<'a>(
        sends: impl Iterator<Item = &'a SendMsg>,
        porf_override: bool,
    ) -> Vec<&'a SendMsg> {
        // Among sends, stamp order respects porf, which includes sb for any model apart from TotalOrder.
        // Therefore, we can detect overwrites in a single forward pass.
        // Note: Amend this is we end up incrementally checking TotalOrder consistency as well.

        let mut sb_min = Vec::new();
        sends.for_each(|s| {
            if Self::is_sb_miminal(s, &sb_min, porf_override) {
                sb_min.push(s)
            }
        });
        sb_min
    }

    /// Returns the coherent matching stores that can be consistently read by recv
    /// when restricting the graph to the view (we exclude one event from the view).
    fn coherent_rfs_in_view(
        &self,
        g: &ExecutionGraph,
        // an optional view, excluding one event (a newly added send)
        view: Option<(&VectorClock, Option<Event>)>,
        recv: &RecvMsg,
        porf_override: bool,
        check_concurrent: bool,
    ) -> Vec<Event> {
        // Sends that the receive can read from
        let sends = g
            .matching_stores(recv.recv_loc())
            // filter-out WakeMsg in our porf prefix: the respective futures were cancelled
            .filter(|&s| !s.is_cancelled_wrt(recv.as_event_label()));

        // Keep those that will exist and be unread after the revisit, checking
        // for concurrent receives.
        let rfs = Self::filter_available_sends_in_view(g, recv, sends, view, check_concurrent);

        // Optional optimization for NoOrder
        let mut rfs: Vec<Event> = if recv.comm() != CommunicationModel::NoOrder {
            // *Assuming* there are no concurrent receives,
            // all existing matching receives are porf-before the current receives.
            // Therefore the consistent sends are exactly the sb-minimal ones.
            Self::retain_sb_minimals(rfs, porf_override)
                .iter()
                .map(|lab| lab.pos())
                .collect()
        } else {
            rfs.map(|lab| lab.pos()).collect()
        };

        // Return them in an arbitrary but fixed order that does
        // *not* depend on the stamps.

        // This is the single place that uses Event's Ord constraint,
        // and *depends* on ThreadId's Ord implementation being stable
        // across executions (i.e. the underlying opaque_id not changing).
        // If this becomes a problem, one can recover a stable, deterministic,
        // ordering on ThreadId's from the execution graph:
        // consider the restriction to Create/Begin events, and use
        // e.g. a dfs pre-order for ordering TheadIds (and by extension, Events).
        rfs.sort();
        rfs
    }

    fn inbox_candidates_in_view(
        &self,
        g: &ExecutionGraph,
        view: Option<(&VectorClock, Option<Event>)>,
        inbox: &Inbox,
        check_concurrent: bool,
    ) -> Vec<Event> {
        // Candidate sends that match the inbox location/predicate.
        let sends = g.matching_stores(inbox.recv_loc());

        let rfs =
            Self::filter_available_sends_in_view_for_inbox(g, inbox, sends, view, check_concurrent);

        // A batch may contain more than one ordered send; check the whole set later.
        let mut rfs: Vec<Event> = rfs.map(|lab| lab.pos()).collect();

        // Stable ordering for canonical subset derivation.
        rfs.sort();
        rfs.dedup();
        rfs
    }

    /// Calculates and populates necessary views for pos
    pub(crate) fn calc_views(&self, g: &mut ExecutionGraph, pos: Event) {
        if pos.index == 0 {
            let mut empty = VectorClock::new();
            empty.set_tid(pos.thread);
            g.label_mut(pos).set_porf_cache(empty.clone());
            g.label_mut(pos).set_posw_cache(empty.clone());
            return;
        }

        let prev = pos.prev();
        let mut porf = g.label(prev).cached_porf().clone();
        let mut posw = g.label(prev).cached_posw().clone();

        porf.update_idx(pos);
        posw.update_idx(pos);

        // Cached views do not include prev's direct dependencies (rf/TCreate/TEnd).
        // Adjust them to do so.

        // rf dependencies
        if let Some(rlab) = g.recv_label(prev) {
            if let Some(rf) = rlab.rf() {
                porf.update(g.label(rf).cached_porf());
                match rlab.comm() {
                    CommunicationModel::TotalOrder => { /* empty */ }
                    // posw does *not* include rf from TotalOrder events
                    _ => posw.update(g.label(rf).cached_posw()),
                }
            }
        }
        if let Some(ilab) = g.inbox_label(prev) {
            if let Some(rfs) = ilab.rfs() {
                for rf in rfs {
                    porf.update(g.label(rf).cached_porf());
                    match ilab.comm() {
                        CommunicationModel::TotalOrder => { /* empty */ }
                        // posw does *not* include rf from TotalOrder events
                        _ => posw.update(g.label(rf).cached_posw()),
                    }
                }
            }
        }

        // TCreate dependencies
        if let LabelEnum::Begin(blab) = g.label(prev) {
            if let Some(parent) = blab.parent() {
                porf.update(g.label(parent).cached_porf());
                // Create -> Begin contributes to sw as well
                posw.update(g.label(parent).cached_posw());
            }
        }

        // TEnd dependencies
        if let LabelEnum::TJoin(jlab) = g.label(prev) {
            porf.update(g.thread_last(jlab.cid()).unwrap().cached_porf());
            // Join -> End contributes to sw as well
            posw.update(g.thread_last(jlab.cid()).unwrap().cached_posw());
        }

        // Set send's sb view
        if let Some(slab) = g.send_label_mut(pos) {
            let mut sb = VectorClock::new();
            match slab.comm() {
                CommunicationModel::NoOrder => { /* empty */ }
                // Local: just include yourself (and po-predecessors)
                CommunicationModel::LocalOrder => sb.set(pos),
                CommunicationModel::CausalOrder => sb.update(&posw),
                // Treat Total similar to Causal, and check full consistency at the end
                CommunicationModel::TotalOrder => sb.update(&porf),
            }
            slab.set_sb(sb);
        }

        // Cache the views
        g.label_mut(pos).set_porf_cache(porf);
        g.label_mut(pos).set_posw_cache(posw);
    }

    // An inbox may take ordered sends together; otherwise each matching
    // predecessor must have been received earlier or lost.
    fn predecessor_order_consistent(&self, g: &ExecutionGraph, sends: &[&SendMsg]) -> bool {
        for &later in sends {
            if !matches!(
                later.comm(),
                CommunicationModel::LocalOrder | CommunicationModel::CausalOrder
            ) {
                continue;
            }
            let Some(later_reader) = later.reader() else {
                continue;
            };

            for &earlier in sends {
                if earlier.pos() == later.pos()
                    || earlier.is_dropped()
                    || !later.sb().contains(earlier.pos())
                    || !Self::reader_matches(g, later_reader, earlier)
                {
                    continue;
                }

                let delivered = match g.label(later_reader) {
                    LabelEnum::RecvMsg(recv) if recv.monitors(earlier) => earlier
                        .monitor_readers()
                        .iter()
                        .any(|&r| r == later_reader || g.in_porf(r, later_reader)),
                    _ => earlier
                        .reader()
                        .is_some_and(|r| r == later_reader || g.in_porf(r, later_reader)),
                };
                if !delivered {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn is_consistent(&self, g: &ExecutionGraph) -> bool {
        // Reuse the complete send list, in graph order, throughout this check.
        let sends: Vec<&SendMsg> = g.all_store_iter().collect();
        if !self.predecessor_order_consistent(g, &sends) {
            return false;
        }

        for &slab1 in &sends {
            if slab1.comm() != CommunicationModel::TotalOrder || slab1.is_dropped() {
                continue;
            }
            for &slab2 in &sends {
                if slab2.comm() != CommunicationModel::TotalOrder || slab2.is_dropped() {
                    continue;
                }

                let s1 = slab1.pos();
                let s2 = slab2.pos();

                if s1 == s2 {
                    continue;
                }

                // For each pair (s1, s2) of sends with TotalOrder

                // s.t. s2 is read by a receive r2
                let r2 = match slab2.reader() {
                    None => continue,
                    Some(r2) => r2,
                };
                // that could have also read s1,
                if !Self::reader_matches(g, r2, slab1) {
                    continue;
                }

                // Sends read by one inbox have the same atomic reader.
                if slab1
                    .reader()
                    .is_some_and(|r1| r1 == r2 || g.in_porf(r1, r2))
                {
                    continue;
                }

                // and s1 is causally_before s2,
                if self.send_before(g, &sends, s1, s2) {
                    // then the execution is inconsistent
                    return false;
                    // because s1 is ordered both
                    // - before s2 (send_before), and
                    // - after s2 (via their respective receives)
                }

                // N.B. We assumed that there are no concurrent receives
                // to reduce "r1 is earlier than r2" to "(r1, r2) in porf".
                // Otherwise, we need to explicitly enumerate linearizations
                // to judge consistency.
            }
        }
        true
    }

    /// Returns whether an affected receive is maximal during a revisit
    pub(crate) fn reads_tiebreaker(
        &self,
        g: &ExecutionGraph,
        rlab: &RecvMsg,
        rev: &Revisit,
        porf_override: bool,
    ) -> bool {
        let view = g.previous_view(rlab.pos(), rev);
        if !g.view_is_porf_closed_except(&view, Some(rlab.pos())) {
            return false;
        }
        // rlab is stamp greater or equal that revisitee's stamp
        assert!(rlab.stamp() >= g.label(rev.pos).stamp());

        // Nonblocking receives are maximal only when they timeout
        if rlab.is_non_blocking() {
            return rlab.rf().is_none();
        }

        // First (non-revisit) is the maximal one.
        // Or this reads from a send currently pointed to a different receive through
        // cancelled-reader fallback during replay.
        let rfs = self.coherent_rfs_in_view(g, Some((&view, None)), rlab, porf_override, false);
        if rfs.is_empty() {
            rlab.rf().is_some_and(|rf| {
                g.send_label(rf)
                    .is_some_and(|slab| slab.reader().is_some_and(|r| r != rlab.pos()))
            })
        } else {
            rlab.rf().unwrap() == rfs[0]
        }
    }

    pub(crate) fn inbox_reads_tiebreaker(
        &self,
        g: &ExecutionGraph,
        ilab: &Inbox,
        rev: &Revisit,
    ) -> bool {
        let mut current = ilab.rfs().unwrap_or_default();
        Self::normalize_event_set(&mut current);
        let view = g.previous_view(ilab.pos(), rev);
        if !g.view_is_porf_closed_except(&view, Some(ilab.pos())) {
            return false;
        }

        let candidates = self.inbox_candidates_in_view(g, Some((&view, None)), ilab, false);
        let mut prefix = g.clone();
        prefix.change_inbox_rfs(ilab.pos(), None);
        prefix.cut_to_view(&view);
        prefix.pop_fallback_readers(ilab.pos());

        self.canonical_inbox_read(&prefix, ilab.pos(), &candidates)
            .is_some_and(|canonical| current == canonical)
    }

    /// Returns the rf options for rlab, with the first being the non-revisit rf step
    pub(crate) fn rfs(
        &self,
        g: &ExecutionGraph,
        rlab: &RecvMsg,
        porf_override: bool,
    ) -> Vec<Event> {
        self.coherent_rfs_in_view(g, None, rlab, porf_override, true)
    }

    pub(crate) fn inbox_candidates(&self, g: &ExecutionGraph, ilab: &Inbox) -> Vec<Event> {
        self.inbox_candidates_in_view(g, None, ilab, true)
    }

    /// Returns whether the resulting execution would be consistent
    ///
    /// Assumes that rlab is not porf-before slab
    pub(crate) fn is_revisit_consistent(
        &self,
        g: &ExecutionGraph,
        rlab: &RecvMsg,
        slab: &SendMsg,
        porf_override: bool,
    ) -> bool {
        assert!(rlab.matches(slab));

        let com = rlab.comm();

        // Optional optimization for NoOrder
        if com == CommunicationModel::NoOrder {
            return true;
        }

        // *Assuming* there are no concurrent receives
        // (which implies that the model is prefix-closed)
        // it suffices to check that slab is not overwritten in the resulting execution

        let rpos = rlab.pos();
        let spos = slab.pos();

        // We disregard the various communication models and check consistency
        // as if everything was CausalOrder.
        let send_sb = if porf_override {
            slab.porf()
        } else {
            slab.sb()
        };

        let view = g.revisit_view(&Revisit::new(rpos, spos));

        let sends = g.matching_stores(rlab.recv_loc()).filter(|&lab| {
            let pos = lab.pos();
            pos != spos && send_sb.contains(pos)
        });

        // if any of them, apart from slab, could be read by rlab after the revisit, then the execution is inconsistent
        let overwritten =
            Self::filter_available_sends_in_view(g, rlab, sends, Some((&view, Some(spos))), false)
                .next()
                .is_none();
        overwritten
    }

    fn well_formed_inbox_placement(
        &self,
        g: &ExecutionGraph,
        inbox_pos: Event,
        sends: &[Event],
        require_min: bool,
    ) -> bool {
        let Some(inbox) = g.inbox_label(inbox_pos) else {
            return false;
        };
        let mut unique = sends.to_vec();
        Self::normalize_event_set(&mut unique);
        if unique.len() != sends.len() {
            return false;
        }
        if let Some(max) = inbox.max() {
            if sends.len() > max {
                return false;
            }
        }
        if require_min && sends.len() < inbox.min() {
            return false;
        }

        // The old read of this inbox may be replaced; other readers may not.
        for &s in sends {
            if !g.contains(s) {
                return false;
            }
            let Some(slab) = g.send_label(s) else {
                return false;
            };
            if slab.is_dropped() || !inbox.matches(slab) {
                return false;
            }
            if slab.reader().is_some_and(|r| r != inbox.pos()) {
                return false;
            }
        }
        !sends.iter().any(|&s| g.in_porf(inbox_pos, s))
    }

    fn inbox_placement_consistent(
        &self,
        g: &ExecutionGraph,
        inbox_pos: Event,
        sends: &[Event],
        require_min: bool,
    ) -> bool {
        if !self.well_formed_inbox_placement(g, inbox_pos, sends, require_min) {
            return false;
        }

        let mut trial = g.clone();
        self.apply_inbox_placement(&mut trial, inbox_pos, sends)
    }

    // Apply a well-formed placement to a disposable graph; rejection leaves it changed.
    fn apply_inbox_placement(
        &self,
        g: &mut ExecutionGraph,
        inbox_pos: Event,
        sends: &[Event],
    ) -> bool {
        let mut chosen = sends.to_vec();
        Self::normalize_event_set(&mut chosen);
        g.change_rf_placement(inbox_pos, &RevisitPlacement::Inbox(chosen));
        self.is_consistent(g)
    }

    pub(crate) fn checked_inbox_placement(
        &self,
        mut g: ExecutionGraph,
        inbox_pos: Event,
        sends: &[Event],
    ) -> Option<ExecutionGraph> {
        if !self.well_formed_inbox_placement(&g, inbox_pos, sends, true) {
            return None;
        }
        self.apply_inbox_placement(&mut g, inbox_pos, sends)
            .then_some(g)
    }

    pub(crate) fn is_inbox_placement_consistent(
        &self,
        g: &ExecutionGraph,
        inbox_pos: Event,
        sends: &[Event],
    ) -> bool {
        self.inbox_placement_consistent(g, inbox_pos, sends, true)
    }

    pub(crate) fn canonical_inbox_read(
        &self,
        g: &ExecutionGraph,
        inbox_pos: Event,
        candidates: &[Event],
    ) -> Option<Vec<Event>> {
        let inbox = g.inbox_label(inbox_pos)?;
        let mut ordered = candidates.to_vec();
        Self::normalize_event_set(&mut ordered);
        let mut chosen = Vec::new();
        while chosen.len() < inbox.min() {
            let next = ordered.iter().copied().find(|&send| {
                if chosen.contains(&send) {
                    return false;
                }
                let mut trial = chosen.clone();
                trial.push(send);
                self.inbox_placement_consistent(g, inbox_pos, &trial, false)
            })?;
            chosen.push(next);
        }
        Self::normalize_event_set(&mut chosen);
        self.is_inbox_placement_consistent(g, inbox_pos, &chosen)
            .then_some(chosen)
    }

    pub(crate) fn inbox_possible_subsets(
        events: &[Event],
        min: usize,
        max: Option<usize>,
        must_include: Option<Event>,
    ) -> Vec<Vec<Event>> {
        fn build(
            idx: usize,
            events: &[Event],
            min: usize,
            max_len: usize,
            must_include: Option<Event>,
            has_must: bool,
            current: &mut Vec<Event>,
            out: &mut Vec<Vec<Event>>,
        ) {
            if current.len() > max_len || current.len() + events.len() - idx < min {
                return;
            }
            if idx == events.len() {
                if current.len() >= min && must_include.is_none_or(|_| has_must) {
                    out.push(current.clone());
                }
                return;
            }
            build(
                idx + 1,
                events,
                min,
                max_len,
                must_include,
                has_must,
                current,
                out,
            );
            current.push(events[idx]);
            build(
                idx + 1,
                events,
                min,
                max_len,
                must_include,
                has_must || must_include == Some(events[idx]),
                current,
                out,
            );
            current.pop();
        }

        let max_len = max.map_or(events.len(), |m| m.min(events.len()));
        if min > max_len || must_include.is_some_and(|s| !events.contains(&s)) {
            return Vec::new();
        }
        let mut subsets = Vec::new();
        build(
            0,
            events,
            min,
            max_len,
            must_include,
            false,
            &mut Vec::new(),
            &mut subsets,
        );
        subsets
    }

    pub(crate) fn normalize_event_set(events: &mut Vec<Event>) {
        // Canonicalize subset representation before comparisons/ownership checks.
        events.sort();
        events.dedup();
    }

    fn reader_matches(g: &ExecutionGraph, reader: Event, send: &SendMsg) -> bool {
        match g.label(reader) {
            LabelEnum::RecvMsg(recv) => recv.matches(send),
            LabelEnum::Inbox(inbox) => inbox.matches(send),
            _ => false,
        }
    }
}
