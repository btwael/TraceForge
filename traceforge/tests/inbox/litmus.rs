use traceforge::{self, Config};

#[derive(Clone, Debug, PartialEq)]
struct Msg {}

macro_rules! equiv_test {
    ($I_case:ident, $R_case:ident) => {
        let I_stats = traceforge::verify(Config::builder().build(), $I_case);
        let R_stats = traceforge::verify(Config::builder().build(), $R_case);
        assert_eq!(I_stats.execs, R_stats.execs);
        assert_eq!(I_stats.block, R_stats.block);
    };
}

macro_rules! not_equiv_test {
    ($I_case:ident, $R_case:ident) => {
        let I_stats = traceforge::verify(Config::builder().build(), $I_case);
        let R_stats = traceforge::verify(Config::builder().build(), $R_case);
        assert!(I_stats.execs != R_stats.execs || I_stats.block != R_stats.block);
    };
}

#[test]
fn I_eq_Rnb() {
    let I_case = move || {
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn I11_eq_Rb() {
    let I_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
    };
    let R_case = move || {
        let _: Msg = traceforge::recv_msg_block();
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn I11_neq_Rnb() {
    let I_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn I_neq_Rb() {
    let I_case = move || {
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        let _: Msg = traceforge::recv_msg_block();
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn SI_eq_SRnb() {
    let I_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _: Option<Msg> = traceforge::recv_msg();
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn IS_eq_RnbS() {
    let I_case = move || {
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn SI_neq_SRb() {
    let I_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _: Msg = traceforge::recv_msg_block();
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn I11S_neq_RnbS() {
    let I_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn test_I2S() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    });
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_SIS() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    });
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2SI() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    });
    assert_eq!(stats.execs, 4);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_nSIjS() {
    for n in 0..4u32 {
        for j in 0..4u32 {
            let stats = traceforge::verify(Config::builder().build(), move || {
                let id = traceforge::thread::main_thread_id();
                for _ in 0..n {
                    traceforge::send_msg(id, Msg {});
                }
                let _ = traceforge::inbox();
                for _ in 0..j {
                    traceforge::send_msg(id, Msg {});
                }
            });
            assert_eq!(stats.execs, (1 + 1u32).pow(n) as usize);
            assert_eq!(stats.block, 0);
        }
    }
}

#[test]
fn test_2SRnbI() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let id = traceforge::thread::main_thread_id();
        for _ in 0..2 {
            traceforge::send_msg(id, Msg {});
        }
        let _: Option<Msg> = traceforge::recv_msg();
        let _ = traceforge::inbox();
    });
    assert_eq!(stats.execs, 8);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_nSRnbI() {
    for n in 0..4u32 {
        let stats = traceforge::verify(Config::builder().build(), move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..n {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox();
        });
        assert_eq!(stats.execs, (2u32.pow(n) + n * 2u32.pow(n - 1)) as usize);
        assert_eq!(stats.block, 0);
    }
}

#[test]
fn test_nSIRnb() {
    for n in 0..4u32 {
        let stats = traceforge::verify(Config::builder().build(), move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..n {
                traceforge::send_msg(id, Msg {});
            }
            let _ = traceforge::inbox();
            let _: Option<Msg> = traceforge::recv_msg();
        });
        assert_eq!(stats.execs, (2u32.pow(n) + n * 2u32.pow(n - 1)) as usize);
        assert_eq!(stats.block, 0);
    }
}
