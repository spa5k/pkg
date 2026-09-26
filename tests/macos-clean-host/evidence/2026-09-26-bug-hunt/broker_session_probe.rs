use pkg_nix::{BrokerOperationKind, InProcessBroker, InProcessCallerPeer, OperationStatus};

fn observe(other_uid: u32) -> (OperationStatus, bool) {
    let broker = InProcessBroker::new().unwrap();
    let owner = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let work = owner.begin(BrokerOperationKind::Build).unwrap();
    owner.acquire_build(&work).unwrap();
    owner.acquire_gc_inhibit(&work).unwrap();
    let observer = broker
        .connect(InProcessCallerPeer::authenticated(other_uid))
        .unwrap();
    let check = observer.begin(BrokerOperationKind::Doctor).unwrap();
    observer.complete(&check).unwrap();
    observer.disconnect().unwrap();
    (
        owner.poll(&work).unwrap(),
        broker.admission_snapshot().build_held(),
    )
}

fn main() {
    let separate_uid = observe(502);
    println!("Different UID disconnect: {separate_uid:?}; expected (Running, true)");
    assert_eq!(separate_uid, (OperationStatus::Running, true));
    let same_uid = observe(501);
    println!("Same UID, separate connection disconnect: {same_uid:?}; expected (Running, true)");
    assert_eq!(
        same_uid,
        (OperationStatus::Running, true),
        "Closing a separate doctor connection must not cancel the build connection"
    );
}
