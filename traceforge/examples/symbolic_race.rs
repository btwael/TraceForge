use traceforge::{
    assert_symbolic, fresh_bool_symbol, recv_msg_block, send_msg, thread, verify, Config, SymBool,
};

fn program() {
    let main_id = thread::current_id();

    // Worker 1
    let t1 = thread::spawn(move || {
        let b1: SymBool = fresh_bool_symbol();
        send_msg(main_id, b1.id());
    });

    // Worker 2
    let t2 = thread::spawn(move || {
        let b2: SymBool = fresh_bool_symbol();
        send_msg(main_id, b2.id());
    });

    // Collector in main thread
    let x: usize = recv_msg_block();
    let y: usize = recv_msg_block();
    
    assert_symbolic(SymBool::from_id(x).eq(SymBool::from_id(y)));

    t1.join().unwrap();
    t2.join().unwrap();
}

fn main() {
    let stats = verify(Config::builder().with_graph_printing("./tex").with_condpor(true).build(), program);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
