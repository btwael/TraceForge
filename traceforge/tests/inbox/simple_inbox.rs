use traceforge::*;

#[derive(Clone, Debug, PartialEq)]
struct Msg {}

fn simple_inbox(inboxes: u32, senders: u32) {
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbx_thread = thread::spawn(move || {
            for i in 0..inboxes {
                let _ = traceforge::inbox();
            }
        });

        let inb_tid = inbx_thread.thread().id();

        let mut sender_threads = Vec::new();

        for i in 0..senders {
            let s = thread::spawn(move || {
                traceforge::send_msg(inb_tid, Msg {});
            });
            sender_threads.push(s);
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbx_thread.join();
    });
    assert_eq!(stats.execs, (inboxes + 1).pow(senders) as usize);
}

macro_rules! simple_inbox_test {
    ($name:ident, $inboxes:expr, $senders:expr) => {
        #[test]
        fn $name() {
            simple_inbox($inboxes, $senders);
        }
    };
}

simple_inbox_test!(simple_inbox_0_0, 0, 0);
simple_inbox_test!(simple_inbox_0_1, 0, 1);
simple_inbox_test!(simple_inbox_0_2, 0, 2);
simple_inbox_test!(simple_inbox_0_3, 0, 3);
simple_inbox_test!(simple_inbox_0_4, 0, 4);
simple_inbox_test!(simple_inbox_0_5, 0, 4);
simple_inbox_test!(simple_inbox_0_6, 0, 6);
simple_inbox_test!(simple_inbox_0_7, 0, 7);
simple_inbox_test!(simple_inbox_0_8, 0, 8);
simple_inbox_test!(simple_inbox_0_9, 0, 9);
simple_inbox_test!(simple_inbox_1_0, 1, 0);
simple_inbox_test!(simple_inbox_1_1, 1, 1);
simple_inbox_test!(simple_inbox_1_2, 1, 2);
simple_inbox_test!(simple_inbox_1_3, 1, 3);
simple_inbox_test!(simple_inbox_1_4, 1, 4);
simple_inbox_test!(simple_inbox_1_5, 1, 4);
simple_inbox_test!(simple_inbox_1_6, 1, 6);
simple_inbox_test!(simple_inbox_1_7, 1, 7);
simple_inbox_test!(simple_inbox_1_8, 1, 8);
simple_inbox_test!(simple_inbox_1_9, 1, 9);
simple_inbox_test!(simple_inbox_2_0, 2, 0);
simple_inbox_test!(simple_inbox_2_1, 2, 1);
simple_inbox_test!(simple_inbox_2_2, 2, 2);
simple_inbox_test!(simple_inbox_2_3, 2, 3);
simple_inbox_test!(simple_inbox_2_4, 2, 4);
simple_inbox_test!(simple_inbox_2_5, 2, 4);
simple_inbox_test!(simple_inbox_2_6, 2, 6);
simple_inbox_test!(simple_inbox_2_7, 2, 7);
simple_inbox_test!(simple_inbox_2_8, 2, 8);
simple_inbox_test!(simple_inbox_2_9, 2, 9);
simple_inbox_test!(simple_inbox_3_0, 3, 0);
simple_inbox_test!(simple_inbox_3_1, 3, 1);
simple_inbox_test!(simple_inbox_3_2, 3, 2);
simple_inbox_test!(simple_inbox_3_3, 3, 3);
simple_inbox_test!(simple_inbox_3_4, 3, 4);
simple_inbox_test!(simple_inbox_3_5, 3, 4);
simple_inbox_test!(simple_inbox_3_6, 3, 6);
simple_inbox_test!(simple_inbox_3_7, 3, 7);
simple_inbox_test!(simple_inbox_3_8, 3, 8);
simple_inbox_test!(simple_inbox_3_9, 3, 9);
simple_inbox_test!(simple_inbox_4_0, 4, 0);
simple_inbox_test!(simple_inbox_4_1, 4, 1);
simple_inbox_test!(simple_inbox_4_2, 4, 2);
simple_inbox_test!(simple_inbox_4_3, 4, 3);
simple_inbox_test!(simple_inbox_4_4, 4, 4);
simple_inbox_test!(simple_inbox_4_5, 4, 4);
simple_inbox_test!(simple_inbox_4_6, 4, 6);
simple_inbox_test!(simple_inbox_4_7, 4, 7);
simple_inbox_test!(simple_inbox_4_8, 4, 8);
//simple_inbox_test!(simple_inbox_4_9, 4, 9);
simple_inbox_test!(simple_inbox_5_0, 5, 0);
simple_inbox_test!(simple_inbox_5_1, 5, 1);
simple_inbox_test!(simple_inbox_5_2, 5, 2);
simple_inbox_test!(simple_inbox_5_3, 5, 3);
simple_inbox_test!(simple_inbox_5_4, 5, 4);
simple_inbox_test!(simple_inbox_5_5, 5, 4);
simple_inbox_test!(simple_inbox_5_6, 5, 6);
simple_inbox_test!(simple_inbox_5_7, 5, 7);
//simple_inbox_test!(simple_inbox_5_8, 5, 8);
//simple_inbox_test!(simple_inbox_5_9, 5, 9);
simple_inbox_test!(simple_inbox_6_0, 6, 0);
simple_inbox_test!(simple_inbox_6_1, 6, 1);
simple_inbox_test!(simple_inbox_6_2, 6, 2);
simple_inbox_test!(simple_inbox_6_3, 6, 3);
simple_inbox_test!(simple_inbox_6_4, 6, 4);
simple_inbox_test!(simple_inbox_6_5, 6, 4);
simple_inbox_test!(simple_inbox_6_6, 6, 6);
//simple_inbox_test!(simple_inbox_6_7, 6, 7);
//simple_inbox_test!(simple_inbox_6_8, 6, 8);
//simple_inbox_test!(simple_inbox_6_9, 6, 9);
//simple_inbox_test!(simple_inbox_7_0, 7, 0);
//simple_inbox_test!(simple_inbox_7_1, 7, 1);
//simple_inbox_test!(simple_inbox_7_2, 7, 2);
//simple_inbox_test!(simple_inbox_7_3, 7, 3);
//simple_inbox_test!(simple_inbox_7_4, 7, 4);
//simple_inbox_test!(simple_inbox_7_5, 7, 4);
//simple_inbox_test!(simple_inbox_7_6, 7, 6);
//simple_inbox_test!(simple_inbox_7_7, 7, 7);
//simple_inbox_test!(simple_inbox_7_8, 7, 8);
//simple_inbox_test!(simple_inbox_7_9, 7, 9);
//simple_inbox_test!(simple_inbox_8_0, 8, 0);
//simple_inbox_test!(simple_inbox_8_1, 8, 1);
//simple_inbox_test!(simple_inbox_8_2, 8, 2);
//simple_inbox_test!(simple_inbox_8_3, 8, 3);
//simple_inbox_test!(simple_inbox_8_4, 8, 4);
//simple_inbox_test!(simple_inbox_8_5, 8, 4);
//simple_inbox_test!(simple_inbox_8_6, 8, 6);
//simple_inbox_test!(simple_inbox_8_7, 8, 7);
//simple_inbox_test!(simple_inbox_8_8, 8, 8);
//simple_inbox_test!(simple_inbox_8_9, 8, 9);
//simple_inbox_test!(simple_inbox_9_0, 9, 0);
//simple_inbox_test!(simple_inbox_9_1, 9, 1);
//simple_inbox_test!(simple_inbox_9_2, 9, 2);
//simple_inbox_test!(simple_inbox_9_3, 9, 3);
//simple_inbox_test!(simple_inbox_9_4, 9, 4);
//simple_inbox_test!(simple_inbox_9_5, 9, 4);
//simple_inbox_test!(simple_inbox_9_6, 9, 6);
//simple_inbox_test!(simple_inbox_9_7, 9, 7);
//simple_inbox_test!(simple_inbox_9_8, 9, 8);
//simple_inbox_test!(simple_inbox_9_9, 9, 9);