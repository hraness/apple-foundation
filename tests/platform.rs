#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_rejects_every_process_entry_before_io() {
    use apple_foundation::{check, schema_check, Bridge, Error};
    use serde_json::json;
    let argv = ["/this/executable/must/not/be/started".to_owned()];
    assert!(matches!(Bridge::new(&argv), Err(Error::Unsupported(_))));
    assert!(matches!(check(&argv), Err(Error::Unsupported(_))));
    assert!(matches!(
        schema_check(&argv, &json!({})),
        Err(Error::Unsupported(_))
    ));
}
