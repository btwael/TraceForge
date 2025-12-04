use traceforge::{self, thread};

const ACCEPTED_TAG: u32 = 2;
const INBOX_CALLS: u32 = 1;

#[test]
fn inbox_filters_on_tag_predicate() {
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let msgs = traceforge::inbox_with_tag(|_, tag| tag == Some(ACCEPTED_TAG));

            assert!(
                msgs.len() <= 1,
                "expected at most one tagged message, got {}",
                msgs.len()
            );

            if let Some(Some(val)) = msgs.get(0) {
                let v = val
                    .as_any_ref()
                    .downcast_ref::<u32>()
                    .expect("expected u32 payload");
                assert_eq!(*v, ACCEPTED_TAG);
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let other_tag = ACCEPTED_TAG + 1;

        let mut senders = Vec::new();
        senders.push(thread::spawn({
            let inbox_tid = inbox_tid.clone();
            move || traceforge::send_tagged_msg(inbox_tid, ACCEPTED_TAG, ACCEPTED_TAG)
        }));
        senders.push(thread::spawn(move || {
            traceforge::send_tagged_msg(inbox_tid, other_tag, other_tag)
        }));

        for sender in senders {
            let _ = sender.join();
        }

        let _ = inbox_thread.join();
    });

    let matching_senders: u32 = 1; // only the ACCEPTED_TAG sender matches the predicate
    let expected_execs = (INBOX_CALLS + 1).pow(matching_senders) as usize;
    assert_eq!(stats.execs, expected_execs);
}
