use traceforge::{
    assert_symbolic, fresh_bool_symbol, recv_msg_block, send_msg, thread, verify, Config, SymBool,
};

fn program() {
    // Thread ids in this execution:
    let main_id = thread::current_id();
    let t2 = thread::spawn(move || {
        let v: usize = recv_msg_block();
        send_msg(main_id, v);
    });

    let t3 = thread::spawn(move || {
        let v: usize = recv_msg_block();
        send_msg(main_id, v);
    });

    let worker2_id = t2.thread().id();
    let worker3_id = t3.thread().id();

    // Broadcaster logic in main thread
    let b: SymBool = fresh_bool_symbol();
    send_msg(worker2_id, b.id());
    send_msg(worker3_id, b.id());

    let x1: usize = recv_msg_block();
    let x2: usize = recv_msg_block();

    assert_eq!(x1, x2);
    assert_symbolic(SymBool::from_id(x1).eq(SymBool::from_id(x2)));

    t2.join().unwrap();
    t3.join().unwrap();
}

fn main() {
    let stats = verify(Config::builder().with_graph_printing("./tex").with_condpor(true).build(), program);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
