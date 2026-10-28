use simeji_engine::arm::ArmMachine;

#[test]
fn executes_arm_function_without_host_ffi() {
    let mut machine = ArmMachine::new().unwrap();
    machine.map(0x1000, 4096).unwrap();
    // add r0, r0, r1; bx lr
    machine
        .write(0x1000, &[0x01, 0x00, 0x80, 0xe0, 0x1e, 0xff, 0x2f, 0xe1])
        .unwrap();
    assert_eq!(machine.call(0x1000, &[17, 25]).unwrap(), 42);
}

#[test]
fn executes_thumb_function() {
    let mut machine = ArmMachine::new().unwrap();
    machine.map(0x1000, 4096).unwrap();
    // adds r0, r0, r1; bx lr
    machine.write(0x1000, &[0x40, 0x18, 0x70, 0x47]).unwrap();
    assert_eq!(machine.call(0x1001, &[17, 25]).unwrap(), 42);
}

#[test]
fn rejects_unmapped_guest_access() {
    let machine = ArmMachine::new().unwrap();
    assert!(machine.read(0x1234_0000, 4).is_err());
}

#[test]
fn rejects_misaligned_and_overflowing_mappings() {
    let mut machine = ArmMachine::new().unwrap();
    assert!(machine.map(1, 4096).is_err());
    assert!(machine.map(0, 0).is_err());
    assert!(machine.map(0xffff_f000, 8192).is_err());
}

#[test]
fn stops_a_guest_that_does_not_return() {
    let mut machine = ArmMachine::new().unwrap();
    machine.map(0x1000, 4096).unwrap();
    // b .
    machine.write(0x1000, &[0xfe, 0xff, 0xff, 0xea]).unwrap();
    let error = machine.call(0x1000, &[]).unwrap_err();
    assert!(error.to_string().contains("execution limits"));
}

#[test]
fn calls_are_not_slowed_by_the_timeout() {
    let mut machine = ArmMachine::new().unwrap();
    machine.map(0x1000, 4096).unwrap();
    machine
        .write(0x1000, &[0x01, 0x00, 0x80, 0xe0, 0x1e, 0xff, 0x2f, 0xe1])
        .unwrap();
    let start = std::time::Instant::now();
    for _ in 0..100 {
        machine.call(0x1000, &[1, 2]).unwrap();
    }
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
}

#[test]
fn passes_fifth_argument_on_the_guest_stack() {
    let mut machine = ArmMachine::new().unwrap();
    machine.map(0x1000, 4096).unwrap();
    // ldr r0, [sp]; bx lr
    machine
        .write(0x1000, &[0x00, 0x00, 0x9d, 0xe5, 0x1e, 0xff, 0x2f, 0xe1])
        .unwrap();
    assert_eq!(machine.call(0x1000, &[1, 2, 3, 4, 42]).unwrap(), 42);
}
