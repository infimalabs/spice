use super::*;

fn driver() -> Arc<Driver<Vec<u8>>> {
    Driver::with_readers(
        Vec::new(),
        || None,
        || None,
        |_| Err("no quotes here".to_owned()),
        steer::Key::for_identity(CanonicalUuid::new(uuid::Uuid::from_u128(7))),
        None,
    )
}

fn notice(position: u64, requested: bool) -> Notice {
    Notice {
        occupant: CanonicalUuid::new(uuid::Uuid::from_u128(7)),
        position,
        requested,
    }
}

fn notices(driver: &Driver<Vec<u8>>) -> Vec<serde_json::Value> {
    let input = driver.stdin.lock().expect("input locks");
    input
        .as_ref()
        .expect("input remains open")
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let frame: serde_json::Value = serde_json::from_slice(line).expect("provider frame");
            let command = frame["message"].as_str().expect("extension command");
            serde_json::from_str(
                command
                    .strip_prefix("/spice-aside ")
                    .expect("private aside command"),
            )
            .expect("private notice")
        })
        .collect()
}

#[test]
fn cancellation_supersedes_preparation_and_blocks_stale_replay() {
    let driver = driver();
    driver.deliver_renewal(notice(2, false));
    driver.deliver_renewal(notice(1, true));
    assert!(notices(&driver).is_empty());

    driver.deliver_renewal(notice(3, true));
    driver.deliver_renewal(notice(4, true));
    driver.deliver_renewal(notice(3, false));
    assert_eq!(notices(&driver).len(), 1);
    assert_eq!(
        *driver.renewal.lock().expect("renewal locks"),
        Some(notice(4, true))
    );

    driver.deliver_renewal(notice(5, false));
    driver.deliver_renewal(notice(4, true));
    let frames = notices(&driver);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["purpose"], "renewal");
    assert_eq!(frames[1]["purpose"], "renewal");
    assert_ne!(frames[0]["message"], frames[1]["message"]);
    assert_eq!(
        *driver.renewal.lock().expect("renewal locks"),
        Some(notice(5, false))
    );

    driver.deliver_renewal(notice(6, true));
    assert_eq!(notices(&driver).len(), 3);
    assert_eq!(
        *driver.renewal.lock().expect("renewal locks"),
        Some(notice(6, true))
    );
}
