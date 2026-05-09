use twitch_1337_web::auth::csrf;

#[test]
fn token_round_trips() {
    let token = [42u8; 32];
    let encoded = csrf::encode(&token);
    let decoded = csrf::decode(&encoded).expect("ok");
    assert_eq!(decoded, token);
}

#[test]
fn verify_accepts_match() {
    let token = [7u8; 32];
    let encoded = csrf::encode(&token);
    assert!(csrf::verify(&encoded, &token));
}

#[test]
fn verify_rejects_mismatch() {
    let token = [7u8; 32];
    let other = [8u8; 32];
    assert!(!csrf::verify(&csrf::encode(&token), &other));
}

#[test]
fn verify_rejects_garbage() {
    assert!(!csrf::verify("not-hex", &[0u8; 32]));
}
