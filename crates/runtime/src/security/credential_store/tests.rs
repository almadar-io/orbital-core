use super::*;

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/credential-store.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture"))
        .expect("fixture json")
}

struct Fixed(Vec<Value>);

impl CredentialRows for Fixed {
    fn list(&self) -> Result<Vec<Value>, String> {
        Ok(self.0.clone())
    }
    fn create(&self, _: &Value) -> Result<String, String> {
        Err("read only".to_string())
    }
    fn update(&self, _: &str, _: &Value) -> Result<(), String> {
        Err("read only".to_string())
    }
    fn delete(&self, _: &str) -> Result<bool, String> {
        Err("read only".to_string())
    }
}

fn js_store(fx: &Value) -> EncryptedCredentialStore {
    let rows = fx["rows"].as_array().expect("rows").clone();
    EncryptedCredentialStore::new(
        Arc::new(Fixed(rows)),
        fx["masterKey"].as_str(),
        fx["previousKey"].as_str(),
    )
}

fn principal_of(case: &Value) -> Option<&str> {
    case["principal"].as_str()
}

#[test]
fn rows_written_by_the_js_store_open_to_the_plaintexts_the_js_store_resolves() {
    let fx = fixture();
    let store = js_store(&fx);
    let expected = fx["resolves"].as_array().expect("resolves");
    assert_eq!(store.warm().expect("warm"), expected.len());
    for case in expected {
        let name = case["name"].as_str().expect("name");
        assert_eq!(
            store.resolve(name, principal_of(case)).as_deref(),
            case["value"].as_str(),
            "{name}"
        );
    }
    for case in expected {
        let name = case["name"].as_str().expect("name");
        assert_eq!(
            store.label(name, principal_of(case)).as_deref(),
            case["label"].as_str(),
            "label of {name}"
        );
    }
    assert_eq!(store.label("GITHUB_TOKEN", None), None);
    let app = store.entries(None);
    let stripe = app
        .iter()
        .find(|e| e.env_var == "STRIPE_KEY")
        .expect("stripe");
    assert_eq!(
        (stripe.service.as_str(), stripe.last4.as_str()),
        ("stripe", "1234")
    );
    let user = store.entries(Some("user-7"));
    assert_eq!(user.len(), 1);
    assert_eq!(user[0].last4, "9876");
}

#[test]
fn the_listing_shows_the_last_four_characters_of_a_secret_of_eight_or_more_and_nothing_for_a_shorter_one() {
    let fx = fixture();
    let store = EncryptedCredentialStore::new(Arc::new(MemoryCredentialRows::default()), fx["masterKey"].as_str(), None);
    store.set("stripe", "LONG", "sk-live-abcd1234", None).expect("set");
    store.set("stripe", "EIGHT", "12345678", None).expect("set");
    store.set("stripe", "SEVEN", "1234567", None).expect("set");
    store.set("stripe", "WIDE", "secret-\u{e9}\u{e8}\u{ea}\u{eb}", None).expect("set");
    let mut tails: Vec<(String, Option<String>)> = store.listing().expect("listing").entries.into_iter().map(|e| (e.name, e.last4)).collect();
    tails.sort();
    assert_eq!(
        tails,
        vec![
            ("EIGHT".to_string(), Some("5678".to_string())),
            ("LONG".to_string(), Some("1234".to_string())),
            ("SEVEN".to_string(), None),
            ("WIDE".to_string(), Some("\u{e9}\u{e8}\u{ea}\u{eb}".to_string())),
        ]
    );
}

#[test]
fn a_label_is_kept_in_the_clear_beside_the_ciphertext_and_is_never_the_secret() {
    let fx = fixture();
    let rows = Arc::new(MemoryCredentialRows::default());
    let store = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
    let secret = "1//refresh-token-SECRET-4242";
    store
        .set_labeled(
            "kflow",
            "KNOWLEDGE_REFRESH_TOKEN",
            secret,
            None,
            Some("ada@example.com"),
        )
        .expect("set");
    assert_eq!(
        store.label("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
        Some("ada@example.com")
    );
    assert_eq!(
        store.resolve("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
        Some(secret)
    );
    let row = rows.list().expect("rows").pop().expect("row");
    assert_eq!(row["label"], "ada@example.com");
    assert!(
        !row.to_string().contains(secret),
        "the stored row never carries the secret in the clear"
    );
    assert!(!row["ciphertext"]
        .as_str()
        .expect("ciphertext")
        .contains("ada@example.com"));
    let reopened = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
    reopened.warm().expect("warm");
    assert_eq!(
        reopened.label("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
        Some("ada@example.com")
    );
    assert_ne!(
        reopened.label("KNOWLEDGE_REFRESH_TOKEN", None).as_deref(),
        Some(secret)
    );
}

#[test]
fn control_a_plain_set_keeps_the_label_and_rotation_keeps_it_and_remove_drops_it() {
    let fx = fixture();
    let rows = Arc::new(MemoryCredentialRows::default());
    let store = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
    store
        .set_labeled("kflow", "T", "first", Some("u1"), Some("ada@example.com"))
        .expect("set");
    store
        .set("kflow", "T", "second", Some("u1"))
        .expect("rotate the value");
    assert_eq!(store.resolve("T", Some("u1")).as_deref(), Some("second"));
    assert_eq!(
        store.label("T", Some("u1")).as_deref(),
        Some("ada@example.com")
    );
    store.rotate().expect("rotate keys");
    let again = EncryptedCredentialStore::new(rows.clone(), fx["masterKey"].as_str(), None);
    again.warm().expect("warm");
    assert_eq!(
        again.label("T", Some("u1")).as_deref(),
        Some("ada@example.com")
    );
    assert_eq!(
        again.label("T", None),
        None,
        "a user row has no app-wide label"
    );
    store
        .set_labeled("kflow", "T", "third", Some("u1"), Some("grace@example.com"))
        .expect("relabel");
    assert_eq!(
        store.label("T", Some("u1")).as_deref(),
        Some("grace@example.com")
    );
    assert!(store.remove("T", Some("u1")).expect("remove"));
    assert_eq!(store.label("T", Some("u1")), None);
}

#[test]
fn control_a_row_without_a_label_reads_as_none_and_an_empty_label_is_refused() {
    let fx = fixture();
    let store = EncryptedCredentialStore::new(
        Arc::new(MemoryCredentialRows::default()),
        fx["masterKey"].as_str(),
        None,
    );
    store.set("kflow", "K", "v", None).expect("set");
    assert_eq!(store.label("K", None), None);
    assert!(store
        .set_labeled("kflow", "K", "v", None, Some(""))
        .is_err());
}

fn row_id(fx: &Value, env_var: &str) -> String {
    fx["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .chain(fx["unreadableRows"].as_array().expect("unreadable rows"))
        .find(|row| row["envVar"] == env_var)
        .expect("row")["id"]
        .as_str()
        .expect("id")
        .to_string()
}

fn rows_with_unreadable(fx: &Value) -> Vec<Value> {
    let mut rows = fx["rows"].as_array().expect("rows").clone();
    rows.extend(
        fx["unreadableRows"]
            .as_array()
            .expect("unreadable rows")
            .clone(),
    );
    rows
}

#[test]
fn a_row_no_held_key_opens_fails_the_warm_naming_its_id_and_the_store_reads_nothing() {
    let fx = fixture();
    let store = EncryptedCredentialStore::new(
        Arc::new(Fixed(rows_with_unreadable(&fx))),
        fx["masterKey"].as_str(),
        fx["previousKey"].as_str(),
    );
    let failure = store.warm().expect_err("an unreadable row fails the warm");
    assert!(failure.contains(&row_id(&fx, "TWILIO_TOKEN")), "{failure}");
    assert!(!failure.contains(&row_id(&fx, "STRIPE_KEY")), "{failure}");
    assert!(!store.warmed());
    assert_eq!(
        store.resolve("STRIPE_KEY", None),
        None,
        "a failed warm leaves nothing to resolve"
    );
    assert!(
        store.set("stripe", "K", "v", None).is_err(),
        "a write waits on a good warm"
    );
}

#[test]
fn a_missing_previous_key_fails_the_warm_naming_the_rows_it_hides() {
    let fx = fixture();
    let store = EncryptedCredentialStore::new(
        Arc::new(Fixed(fx["rows"].as_array().expect("rows").clone())),
        fx["masterKey"].as_str(),
        None,
    );
    let failure = store
        .warm()
        .expect_err("a row under the retired key is unreadable");
    assert!(failure.contains(&row_id(&fx, "GITHUB_TOKEN")), "{failure}");
    assert!(!failure.contains(&row_id(&fx, "STRIPE_KEY")), "{failure}");
}

#[test]
fn control_malformed_and_unknown_kind_rows_fail_the_warm_and_so_does_a_retired_derived_row() {
    let fx = fixture();
    let readable = fx["rows"].as_array().expect("rows").clone();
    let with = |change: &dyn Fn(&mut Value)| {
        let mut rows = readable.clone();
        change(&mut rows[0]);
        let id = rows[0]["id"].as_str().expect("id").to_string();
        let store = EncryptedCredentialStore::new(
            Arc::new(Fixed(rows)),
            fx["masterKey"].as_str(),
            fx["previousKey"].as_str(),
        );
        (store.warm(), id)
    };
    for change in [
        (&|row: &mut Value| row["iv"] = json!("")) as &dyn Fn(&mut Value),
        &|row: &mut Value| row["authTag"] = json!("zz"),
        &|row: &mut Value| row["ciphertext"] = json!(7),
        &|row: &mut Value| row["principal"] = json!(""),
        &|row: &mut Value| row["kind"] = json!("mystery"),
        &|row: &mut Value| row["id"] = json!(""),
    ] {
        let (warm, id) = with(change);
        let failure = warm.expect_err("a malformed row fails the warm");
        assert!(failure.contains("1 credential row(s)"), "{failure}");
        if !id.is_empty() {
            assert!(failure.contains(&id), "{failure}");
        }
    }
    let (retired, id) = with(&|row: &mut Value| row["kind"] = json!("derived"));
    let failure = retired.expect_err("a row of the retired derived kind is a refusal, not a credential");
    assert!(failure.contains(&id) && failure.contains("retired kind `derived`"), "{failure}");
}

#[test]
fn control_a_user_row_is_bound_to_its_principal_and_stripped_of_it_it_fails_the_warm() {
    let fx = fixture();
    let mut rows = fx["rows"].as_array().expect("rows").clone();
    let mine = rows
        .iter()
        .position(|r| r["envVar"] == "OPENAI_KEY")
        .expect("row");
    rows[mine].as_object_mut().expect("row").remove("principal");
    let store = EncryptedCredentialStore::new(
        Arc::new(Fixed(rows)),
        fx["masterKey"].as_str(),
        fx["previousKey"].as_str(),
    );
    let failure = store
        .warm()
        .expect_err("the row no longer opens as the app credential");
    assert!(failure.contains(&row_id(&fx, "OPENAI_KEY")), "{failure}");
}

#[test]
fn fixed_iv_seals_are_byte_for_byte_the_ones_node_crypto_makes() {
    let fx = fixture();
    for case in fx["seals"].as_array().expect("seals") {
        let iv: [u8; 12] = unhex(case["ivHex"].as_str().expect("iv"))
            .expect("hex")
            .try_into()
            .expect("12 bytes");
        let store = EncryptedCredentialStore::with_iv(
            Arc::new(MemoryCredentialRows::default()),
            fx["masterKey"].as_str(),
            None,
            Box::new(move || iv),
        );
        let sealed = store
            .seal(
                case["plaintext"].as_str().expect("plain"),
                case["name"].as_str().expect("name"),
                principal_of(case),
            )
            .expect("seal");
        assert_eq!(
            sealed.ciphertext,
            case["ciphertext"].as_str().expect("ct"),
            "{}",
            case["name"]
        );
        assert_eq!(sealed.auth_tag, case["authTag"].as_str().expect("tag"));
        assert_eq!(sealed.key_id, case["keyId"].as_str().expect("key id"));
    }
}

fn temp_file(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("credential-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("creds.json")
}

#[test]
fn the_file_store_round_trips_and_keeps_the_js_row_shape() {
    let fx = fixture();
    let path = temp_file("roundtrip");
    let store = EncryptedCredentialStore::new(
        Arc::new(FileCredentialRows::new(&path)),
        fx["masterKey"].as_str(),
        None,
    );
    let entry = store
        .set("stripe", "STRIPE_KEY", "fake-stripe-4321", None)
        .expect("set");
    assert_eq!(entry.last4, "4321");
    store
        .set("openai", "OPENAI_KEY", "per-user-0001", Some("user-9"))
        .expect("set user");
    let again = EncryptedCredentialStore::new(
        Arc::new(FileCredentialRows::new(&path)),
        fx["masterKey"].as_str(),
        None,
    );
    assert_eq!(again.warm().expect("warm"), 2);
    assert_eq!(
        again.resolve("STRIPE_KEY", None).as_deref(),
        Some("fake-stripe-4321")
    );
    assert_eq!(
        again.resolve("OPENAI_KEY", Some("user-9")).as_deref(),
        Some("per-user-0001")
    );
    let text = std::fs::read_to_string(&path).expect("file");
    let rows: Vec<Value> = serde_json::from_str(&text).expect("rows");
    let row = rows
        .iter()
        .find(|r| r["envVar"] == "STRIPE_KEY")
        .expect("row");
    for field in [
        "id",
        "service",
        "envVar",
        "ciphertext",
        "iv",
        "authTag",
        "keyId",
        "updatedAt",
    ] {
        assert!(!row[field].is_null(), "{field}");
    }
    assert!(row.get("principal").is_none());
    assert!(!text.contains("fake-stripe-4321"));
    assert!(again.remove("STRIPE_KEY", None).expect("remove"));
    assert!(!again.remove("STRIPE_KEY", None).expect("remove twice"));
}

#[test]
fn rotation_re_encrypts_rows_under_the_previous_key_and_counts_the_rest() {
    let fx = fixture();
    let rows = Arc::new(MemoryCredentialRows::default());
    rows.load(fx["rows"].as_array().expect("rows").clone());
    let store = EncryptedCredentialStore::new(
        rows.clone(),
        fx["masterKey"].as_str(),
        fx["previousKey"].as_str(),
    );
    let counts = store.rotate().expect("rotate");
    assert_eq!(
        counts,
        RotateCounts {
            rotated: 1,
            already_current: 3,
            unreadable: 0
        }
    );
    let retired = EncryptedCredentialStore::new(rows, fx["masterKey"].as_str(), None);
    retired.warm().expect("warm");
    assert_eq!(
        retired.resolve("GITHUB_TOKEN", None).as_deref(),
        Some("fake-github-5555")
    );
}

#[test]
fn control_a_rotation_over_an_unreadable_row_fails_naming_it_and_touches_no_row() {
    let fx = fixture();
    let rows = Arc::new(MemoryCredentialRows::default());
    rows.load(rows_with_unreadable(&fx));
    let before = rows.list().expect("rows");
    let store = EncryptedCredentialStore::new(
        rows.clone(),
        fx["masterKey"].as_str(),
        fx["previousKey"].as_str(),
    );
    let failure = store
        .rotate()
        .expect_err("an unreadable row fails the rotation");
    assert!(failure.contains(&row_id(&fx, "TWILIO_TOKEN")), "{failure}");
    assert_eq!(
        rows.list().expect("rows"),
        before,
        "no row was re-encrypted before the failure"
    );
}

#[test]
fn without_a_master_key_nothing_is_stored_or_read() {
    let fx = fixture();
    let store = EncryptedCredentialStore::new(
        Arc::new(MemoryCredentialRows::default()),
        Some("not-hex"),
        None,
    );
    assert!(!store.enabled());
    assert_eq!(store.warm().expect("warm"), 0);
    assert!(store.set("stripe", "K", "v", None).is_err());
    assert!(store.rotate().is_err());
    let listing = store.listing().expect("listing");
    assert!(!listing.enabled && listing.entries.is_empty());
    assert!(js_store(&fx).enabled());
}

#[test]
fn rows_kept_in_the_apps_store_survive_a_new_store_and_resolve_the_same_value() {
    let fx = fixture();
    let persistence = Arc::new(crate::effects::executor::InMemoryPersistence::new());
    let rows = || Arc::new(StoreCredentialRows(persistence.clone())) as Arc<dyn CredentialRows>;
    let store = EncryptedCredentialStore::new(rows(), fx["masterKey"].as_str(), None);
    store
        .set("stripe", "STRIPE_KEY", "sk_live_1234", None)
        .expect("set");
    let reopened = EncryptedCredentialStore::new(rows(), fx["masterKey"].as_str(), None);
    reopened.warm().expect("warm");
    assert_eq!(
        reopened.resolve("STRIPE_KEY", None).as_deref(),
        Some("sk_live_1234")
    );
    let kept = crate::effects::executor::PersistenceHandler::list(
        persistence.as_ref(),
        CREDENTIAL_ENTITY_TYPE,
    )
    .unwrap();
    let row = serde_json::to_value(&kept[0]).unwrap();
    assert!(
        row["ciphertext"].is_string() && row.get("value").is_none(),
        "the store keeps ciphertext, never the value: {row}"
    );
    assert!(reopened.remove("STRIPE_KEY", None).expect("remove"));
    assert!(crate::effects::executor::PersistenceHandler::list(
        persistence.as_ref(),
        CREDENTIAL_ENTITY_TYPE
    )
    .unwrap()
    .is_empty());
}

#[test]
fn control_an_update_of_a_missing_row_leaves_it_missing() {
    let persistence = Arc::new(crate::effects::executor::InMemoryPersistence::new());
    let rows = StoreCredentialRows(persistence.clone());
    rows.update("absent", &json!({ "envVar": "X" }))
        .expect("a missing row is not an error");
    assert!(rows.list().unwrap().is_empty());
}

fn store_over(rows: Arc<dyn CredentialRows>) -> EncryptedCredentialStore {
    EncryptedCredentialStore::new(rows, fixture()["masterKey"].as_str(), None)
}

#[test]
fn recorded_rows_hand_a_host_the_writes_to_apply_and_a_second_store_over_them_reads_the_same_credentials(
) {
    let recorded = Arc::new(RecordedCredentialRows::default());
    let store = store_over(recorded.clone());
    store.warm().expect("warm");
    store
        .set("stripe", "STRIPE_KEY", "sk-1111", None)
        .expect("set");
    store
        .set("stripe", "STRIPE_KEY", "sk-2222", None)
        .expect("rotate the value");
    store
        .set("openai", "OPENAI_KEY", "per-user-0001", Some("u1"))
        .expect("set user");
    assert!(store.remove("OPENAI_KEY", Some("u1")).expect("remove"));
    let writes = recorded.take_writes();
    let kinds: Vec<&str> = writes
        .iter()
        .map(|w| w["write"].as_str().expect("write"))
        .collect();
    assert_eq!(kinds, ["create", "update", "create", "delete"]);
    let medium = Arc::new(MemoryCredentialRows::default());
    for write in &writes {
        let id = write["id"].as_str().expect("id");
        match write["write"].as_str().expect("write") {
            "create" => {
                medium.load({
                    let mut held = medium.list().expect("rows");
                    held.push(write["data"].clone());
                    held
                });
            }
            "update" => medium.update(id, &write["data"]).expect("update"),
            _ => {
                medium.delete(id).expect("delete");
            }
        }
    }
    let reopened = store_over(medium);
    assert_eq!(reopened.warm().expect("warm"), 1);
    assert_eq!(
        reopened.resolve("STRIPE_KEY", None).as_deref(),
        Some("sk-2222")
    );
    assert!(
        recorded.take_writes().is_empty(),
        "taking the writes empties the record"
    );
}

#[test]
fn control_loading_rows_drops_writes_the_host_never_applied_and_an_update_of_a_missing_row_records_nothing(
) {
    let recorded = Arc::new(RecordedCredentialRows::default());
    recorded.create(&json!({ "envVar": "A" })).expect("create");
    recorded.load(Vec::new());
    assert!(recorded.take_writes().is_empty());
    recorded
        .update("absent", &json!({ "envVar": "B" }))
        .expect("update");
    assert!(!recorded.delete("absent").expect("delete"));
    assert!(recorded.take_writes().is_empty());
}

#[cfg(unix)]
#[test]
fn the_file_store_is_written_readable_by_its_owner_alone_on_every_save() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let path = temp_file("mode");
    let store = EncryptedCredentialStore::new(
        Arc::new(FileCredentialRows::new(&path)),
        fx["masterKey"].as_str(),
        None,
    );
    store.set("stripe", "STRIPE_KEY", "fake-stripe-4321", None).expect("set");
    let mode = |p: &std::path::Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600, "the first save creates the file private");
    store.set("openai", "OPENAI_KEY", "per-user-0001", None).expect("set again");
    assert_eq!(mode(&path), 0o600, "a later save keeps it private");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");
    store.set("third", "THIRD_KEY", "per-user-0002", None).expect("set over a loosened file");
    assert_eq!(mode(&path), 0o600, "a file someone loosened is private again after the next save");
    assert!(!path.with_extension("json.tmp").exists(), "the private temp file was renamed away");
}

#[test]
fn a_master_key_wipes_its_bytes_when_it_drops() {
    let mut key = Key::parse(&"ab".repeat(32)).expect("key");
    assert_eq!(key.bytes, [0xab; 32]);
    zeroize::Zeroize::zeroize(&mut key.bytes);
    assert_eq!(key.bytes, [0u8; 32]);
    let mut stored = Stored {
        id: "i".into(),
        entry: CredentialEntry { service: "s".into(), env_var: "E".into(), last4: "1234".into(), updated_at: 0, principal: None },
        value: "plaintext-secret".into(),
        label: None,
        oauth: None,
    };
    zeroize::Zeroize::zeroize(&mut stored.value);
    assert!(stored.value.is_empty(), "a zeroized String is emptied after its bytes are overwritten");
}

mod oauth_connect {
    use super::*;
    use crate::credential_admin::{ClientAuth, ConnectFailureKind, CredentialGrant, CredentialStanding};
    use std::sync::Mutex as StdMutex;

    struct Fake {
        reply: Result<(u16, String), TransportError>,
        seen: StdMutex<Vec<(String, Vec<(String, String)>, String)>>,
    }

    impl Fake {
        fn answering(status: u16, body: &str) -> Self {
            Self { reply: Ok((status, body.to_string())), seen: StdMutex::default() }
        }
    }

    impl CredentialTransport for Fake {
        fn post_form(&self, endpoint: &str, headers: &[(String, String)], body: &str) -> Result<(u16, String), TransportError> {
            self.seen.lock().unwrap().push((endpoint.to_string(), headers.to_vec(), body.to_string()));
            self.reply.clone()
        }
    }

    const GOOD: &str = r#"{"access_token":"ACCESS-TOKEN-7777","token_type":"Bearer","refresh_token":"REFRESH-TOKEN-8888","expires_in":3600}"#;

    fn store() -> (Arc<MemoryCredentialRows>, EncryptedCredentialStore) {
        let rows = Arc::new(MemoryCredentialRows::default());
        let store = EncryptedCredentialStore::new(rows.clone(), Some(&"ab".repeat(32)), None);
        store.set("google", "CLIENT_ID", "client-id-1", None).unwrap();
        store.set("google", "CLIENT_SECRET", "client-secret-2", None).unwrap();
        (rows, store)
    }

    fn grant(principal: Option<&str>, auth: ClientAuth) -> CredentialGrant {
        CredentialGrant {
            name: "DRIVE".to_string(),
            principal: principal.map(str::to_string),
            token_endpoint: "https://auth.example/token".to_string(),
            grant: "authorization_code".to_string(),
            code: "one-time-CODE".into(),
            redirect_uri: "https://app.example/cb".to_string(),
            verifier: Some("pkce-VERIFIER".into()),
            client_id: "CLIENT_ID".to_string(),
            client_secret: Some("CLIENT_SECRET".to_string()),
            client_auth: auth,
            scopes: vec![],
        }
    }

    #[test]
    fn success_persists_an_oauth_row_per_principal_and_answers_only_connected_and_label() {
        let (rows, store) = store();
        let fake = Fake::answering(200, GOOD);
        let standing = store.connect(&grant(Some("u1"), ClientAuth::Body), &fake).unwrap();
        assert_eq!(standing, CredentialStanding { connected: true, label: None });
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].0, "https://auth.example/token");
        assert!(seen[0].2.contains("client_id=client-id-1") && seen[0].2.contains("client_secret=client-secret-2"), "the store resolved the client");
        assert!(seen[0].2.contains("code=one-time-CODE"));
        assert_eq!(store.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-TOKEN-7777"), "secret_of answers the access token");
        assert_eq!(store.resolve("DRIVE", Some("u2")), None);
        assert_eq!(store.resolve("DRIVE", None), None);
        let row = rows.list().unwrap().into_iter().find(|row| row["envVar"] == "DRIVE").expect("row");
        assert_eq!(row["kind"], "oauth");
        let shown = serde_json::to_string(&standing_json(&standing)).unwrap();
        assert!(!shown.contains("TOKEN"), "the answer carries no token: {shown}");
        assert!(!rows.list().unwrap().iter().any(|row| row.to_string().contains("TOKEN-")), "rows hold ciphertext only");
        let restarted = EncryptedCredentialStore::new(rows, Some(&"ab".repeat(32)), None);
        restarted.warm().unwrap();
        assert_eq!(restarted.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-TOKEN-7777"), "the row survives a restart");
        assert!(store.entries(Some("u1")).iter().all(|entry| entry.last4.is_empty()), "no token tail is shown");
    }

    fn standing_json(standing: &CredentialStanding) -> Value {
        serde_json::to_value(standing).unwrap()
    }

    #[test]
    fn basic_auth_sends_the_resolved_client_in_the_header() {
        let (_, store) = store();
        let fake = Fake::answering(200, GOOD);
        store.connect(&grant(None, ClientAuth::Basic), &fake).unwrap();
        let seen = fake.seen.lock().unwrap();
        let header = seen[0].1.iter().find(|(k, _)| k == "Authorization").expect("header");
        assert_eq!(header.1, format!("Basic {}", orbital_core_base::codec::base64_encode(b"client-id-1:client-secret-2")));
        assert!(!seen[0].2.contains("client_secret"));
    }

    #[test]
    fn failures_map_to_their_kinds_and_persist_nothing() {
        let cases = [
            (200, r#"{"error":"invalid_grant"}"#, ConnectFailureKind::InvalidGrant),
            (401, r#"{"error":"invalid_client"}"#, ConnectFailureKind::InvalidClient),
            (200, r#"{"access_token":"LEAK-ME","token_type":"mac"}"#, ConnectFailureKind::Malformed),
            (502, "bad gateway", ConnectFailureKind::Malformed),
        ];
        for (status, body, kind) in cases {
            let (rows, store) = store();
            let before = rows.list().unwrap().len();
            let failure = store.connect(&grant(Some("u1"), ClientAuth::Body), &Fake::answering(status, body)).unwrap_err();
            assert_eq!(failure.kind, kind, "{body}");
            assert!(!failure.message.contains("LEAK-ME"));
            assert_eq!(rows.list().unwrap().len(), before, "{body}: nothing persisted");
            assert_eq!(store.resolve("DRIVE", Some("u1")), None);
        }
    }

    #[test]
    fn a_transport_error_is_transport_and_a_missing_client_credential_is_refused_before_any_post() {
        let (rows, store) = store();
        let down = Fake { reply: Err(TransportError("egress denied".into())), seen: StdMutex::default() };
        let failure = store.connect(&grant(None, ClientAuth::Body), &down).unwrap_err();
        assert_eq!((failure.kind, failure.message.as_str()), (ConnectFailureKind::Transport, "egress denied"));
        let mut missing = grant(None, ClientAuth::Body);
        missing.client_id = "NOPE".to_string();
        let silent = Fake::answering(200, GOOD);
        assert_eq!(store.connect(&missing, &silent).unwrap_err().kind, ConnectFailureKind::Refused);
        assert!(silent.seen.lock().unwrap().is_empty());
        assert_eq!(rows.list().unwrap().len(), 2);
    }

    #[test]
    fn a_store_without_a_master_key_is_unavailable() {
        let store = EncryptedCredentialStore::new(Arc::new(MemoryCredentialRows::default()), None, None);
        let failure = store.connect(&grant(None, ClientAuth::Body), &Fake::answering(200, GOOD)).unwrap_err();
        assert_eq!(failure.kind, ConnectFailureKind::Unavailable);
    }

    #[test]
    fn connecting_over_a_plain_row_replaces_it_and_setting_over_an_oauth_row_replaces_that() {
        let (rows, store) = store();
        store.set("x", "DRIVE", "plain-value-1", Some("u1")).unwrap();
        store.connect(&grant(Some("u1"), ClientAuth::Body), &Fake::answering(200, GOOD)).unwrap();
        assert_eq!(store.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-TOKEN-7777"));
        store.set("x", "DRIVE", "plain-value-2", Some("u1")).unwrap();
        let fresh = EncryptedCredentialStore::new(rows.clone(), Some(&"ab".repeat(32)), None);
        fresh.warm().expect("every row readable");
        assert_eq!(fresh.resolve("DRIVE", Some("u1")).as_deref(), Some("plain-value-2"));
        assert_eq!(rows.list().unwrap().iter().filter(|row| row["envVar"] == "DRIVE").count(), 1);
    }

    #[test]
    fn the_credentials_host_connects_through_its_transport_and_is_unavailable_without_one() {
        use crate::secret_source::SecretSource;
        let (_, store) = store();
        let store = Arc::new(store);
        let bare = crate::credentials::Credentials::default().with_encrypted_store(store.clone()).unwrap();
        assert_eq!(bare.connect_of(&grant(Some("u1"), ClientAuth::Body)).unwrap_err().kind, ConnectFailureKind::Unavailable);
        let host = bare.with_grant_transport(Arc::new(Fake::answering(200, GOOD)));
        let standing = host.connect_of(&grant(Some("u1"), ClientAuth::Body)).unwrap();
        assert!(standing.connected);
        assert_eq!(host.secret_of("DRIVE", Some("u1")), Ok(Some("ACCESS-TOKEN-7777".to_string())));
    }

    mod refresh {
        use super::*;
        use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

        struct Scripted {
            replies: StdMutex<Vec<Result<(u16, String), TransportError>>>,
            calls: AtomicUsize,
            bodies: StdMutex<Vec<String>>,
        }

        impl Scripted {
            fn new(replies: Vec<Result<(u16, String), TransportError>>) -> Self {
                Self { replies: StdMutex::new(replies), calls: AtomicUsize::new(0), bodies: StdMutex::default() }
            }
            fn ok(body: &str) -> Result<(u16, String), TransportError> {
                Ok((200, body.to_string()))
            }
        }

        impl CredentialTransport for Scripted {
            fn post_form(&self, _: &str, _: &[(String, String)], body: &str) -> Result<(u16, String), TransportError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.bodies.lock().unwrap().push(body.to_string());
                std::thread::sleep(std::time::Duration::from_millis(40));
                let mut replies = self.replies.lock().unwrap();
                assert!(!replies.is_empty(), "an unexpected extra refresh");
                replies.remove(0)
            }
        }

        const ROTATED: &str = r#"{"access_token":"ACCESS-2","token_type":"Bearer","refresh_token":"REFRESH-2","expires_in":3600}"#;
        const KEPT: &str = r#"{"access_token":"ACCESS-3","token_type":"Bearer","expires_in":3600}"#;

        fn clocked() -> (Arc<AtomicI64>, Arc<MemoryCredentialRows>, Arc<EncryptedCredentialStore>) {
            let now = Arc::new(AtomicI64::new(1_000_000));
            let rows = Arc::new(MemoryCredentialRows::default());
            let clock = now.clone();
            let store = EncryptedCredentialStore::new(rows.clone(), Some(&"ab".repeat(32)), None)
                .with_clock(Box::new(move || clock.load(Ordering::SeqCst)));
            store.set("google", "CLIENT_ID", "client-id-1", None).unwrap();
            store.set("google", "CLIENT_SECRET", "client-secret-2", None).unwrap();
            (now, rows, Arc::new(store))
        }

        fn connected() -> (Arc<AtomicI64>, Arc<MemoryCredentialRows>, Arc<EncryptedCredentialStore>) {
            let (now, rows, store) = clocked();
            store.connect(&grant(Some("u1"), ClientAuth::Body), &Fake::answering(200, GOOD)).unwrap();
            (now, rows, store)
        }

        fn expire(now: &AtomicI64) {
            now.fetch_add(3_600_000, Ordering::SeqCst);
        }

        #[test]
        fn a_token_that_is_not_expired_is_answered_without_a_transport_call() {
            let (_, _, store) = connected();
            let transport = Scripted::new(vec![]);
            assert_eq!(store.resolve_fresh("DRIVE", Some("u1"), Some(&transport)).unwrap().as_deref(), Some("ACCESS-TOKEN-7777"));
            assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        }

        #[test]
        fn an_expired_token_refreshes_once_under_eight_concurrent_resolves_and_rotation_persists() {
            let (now, rows, store) = connected();
            expire(&now);
            let transport = Arc::new(Scripted::new(vec![Scripted::ok(ROTATED)]));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (store, transport) = (store.clone(), transport.clone());
                    std::thread::spawn(move || store.resolve_fresh("DRIVE", Some("u1"), Some(transport.as_ref())).unwrap())
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().unwrap().as_deref(), Some("ACCESS-2"));
            }
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
            let body = transport.bodies.lock().unwrap()[0].clone();
            assert!(body.starts_with("grant_type=refresh_token&refresh_token=REFRESH-TOKEN-8888"), "{body}");
            assert!(body.contains("client_id=client-id-1"));
            let restarted = EncryptedCredentialStore::new(rows, Some(&"ab".repeat(32)), None);
            restarted.warm().unwrap();
            assert_eq!(restarted.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-2"));
            let next = Scripted::new(vec![Scripted::ok(KEPT)]);
            expire(&now);
            assert_eq!(store.resolve_fresh("DRIVE", Some("u1"), Some(&next)).unwrap().as_deref(), Some("ACCESS-3"));
            assert!(next.bodies.lock().unwrap()[0].contains("refresh_token=REFRESH-2"), "the rotated token is the one used next");
        }

        #[test]
        fn a_refresh_answer_without_a_new_refresh_token_keeps_the_old_one() {
            let (now, _, store) = connected();
            expire(&now);
            store.resolve_fresh("DRIVE", Some("u1"), Some(&Scripted::new(vec![Scripted::ok(KEPT)]))).unwrap();
            expire(&now);
            let next = Scripted::new(vec![Scripted::ok(ROTATED)]);
            store.resolve_fresh("DRIVE", Some("u1"), Some(&next)).unwrap();
            assert!(next.bodies.lock().unwrap()[0].contains("refresh_token=REFRESH-TOKEN-8888"));
        }

        #[test]
        fn invalid_grant_drops_the_row_and_the_status_is_then_not_connected() {
            let (now, rows, store) = connected();
            expire(&now);
            let transport = Scripted::new(vec![Ok((400, r#"{"error":"invalid_grant"}"#.to_string()))]);
            assert_eq!(store.resolve_fresh("DRIVE", Some("u1"), Some(&transport)).unwrap(), None);
            assert_eq!(store.resolve("DRIVE", Some("u1")), None);
            assert!(!rows.list().unwrap().iter().any(|row| row["envVar"] == "DRIVE"));
            assert_eq!(store.resolve_fresh("DRIVE", Some("u1"), Some(&Scripted::new(vec![]))).unwrap(), None);
        }

        #[test]
        fn a_transport_failure_keeps_the_row_and_returns_a_typed_error_without_token_text() {
            let (now, _, store) = connected();
            expire(&now);
            let down = Scripted::new(vec![Err(TransportError("egress denied".into()))]);
            let failure = store.resolve_fresh("DRIVE", Some("u1"), Some(&down)).unwrap_err();
            assert_eq!((failure.kind, failure.message.as_str()), (ConnectFailureKind::Transport, "egress denied"));
            assert!(!failure.message.contains("TOKEN"));
            assert_eq!(store.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-TOKEN-7777"), "the row is kept");
            let up = Scripted::new(vec![Scripted::ok(ROTATED)]);
            assert_eq!(store.resolve_fresh("DRIVE", Some("u1"), Some(&up)).unwrap().as_deref(), Some("ACCESS-2"));
        }

        #[test]
        fn an_expired_row_with_no_refresh_token_answers_not_held() {
            let (now, _, store) = clocked();
            let body = r#"{"access_token":"ONLY-ACCESS","token_type":"Bearer","expires_in":3600}"#;
            store.connect(&grant(Some("u1"), ClientAuth::Body), &Fake::answering(200, body)).unwrap();
            expire(&now);
            let transport = Scripted::new(vec![]);
            assert_eq!(store.resolve_fresh("DRIVE", Some("u1"), Some(&transport)).unwrap(), None);
            assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        }

        fn refresh_grant(auth: ClientAuth) -> CredentialGrant {
            let mut ask = grant(Some("u1"), auth);
            ask.grant = "refresh_token".to_string();
            ask.code = "".into();
            ask.redirect_uri = String::new();
            ask.verifier = None;
            ask
        }

        #[test]
        fn the_explicit_refresh_grant_exchanges_the_stored_refresh_token_and_answers_connected() {
            let (_, rows, store) = connected();
            let transport = Scripted::new(vec![Scripted::ok(ROTATED)]);
            let standing = store.connect(&refresh_grant(ClientAuth::Body), &transport).unwrap();
            assert_eq!(standing, CredentialStanding { connected: true, label: None });
            assert!(transport.bodies.lock().unwrap()[0].contains("refresh_token=REFRESH-TOKEN-8888"));
            assert_eq!(store.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-2"));
            let restarted = EncryptedCredentialStore::new(rows, Some(&"ab".repeat(32)), None);
            restarted.warm().unwrap();
            assert_eq!(restarted.resolve("DRIVE", Some("u1")).as_deref(), Some("ACCESS-2"));
        }

        #[test]
        fn the_explicit_refresh_grant_without_a_stored_refresh_token_fails_typed_and_posts_nothing() {
            let (_, _, store) = clocked();
            let transport = Scripted::new(vec![]);
            let none = store.connect(&refresh_grant(ClientAuth::Body), &transport).unwrap_err();
            assert_eq!(none.kind, ConnectFailureKind::InvalidGrant);
            let body = r#"{"access_token":"ONLY-ACCESS","token_type":"Bearer"}"#;
            store.connect(&grant(Some("u1"), ClientAuth::Body), &Fake::answering(200, body)).unwrap();
            let held = store.connect(&refresh_grant(ClientAuth::Body), &transport).unwrap_err();
            assert_eq!(held.kind, ConnectFailureKind::InvalidGrant);
            assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
            assert_eq!(store.resolve("DRIVE", Some("u1")).as_deref(), Some("ONLY-ACCESS"));
        }

        #[test]
        fn an_expired_app_scope_row_refreshes_on_secret_and_a_live_one_makes_no_call() {
            use crate::secret_source::SecretSource;
            let (now, _, store) = clocked();
            store.connect(&grant(None, ClientAuth::Body), &Fake::answering(200, GOOD)).unwrap();
            let live = Arc::new(Scripted::new(vec![]));
            let host = crate::credentials::Credentials::default()
                .with_encrypted_store(store.clone())
                .unwrap()
                .with_grant_transport(live.clone());
            assert_eq!(host.secret("DRIVE"), Ok(Some("ACCESS-TOKEN-7777".to_string())));
            assert_eq!(live.calls.load(Ordering::SeqCst), 0);
            expire(&now);
            let due = Arc::new(Scripted::new(vec![Scripted::ok(ROTATED)]));
            let host = host.with_grant_transport(due.clone());
            assert_eq!(host.secret("DRIVE"), Ok(Some("ACCESS-2".to_string())));
            assert_eq!(due.calls.load(Ordering::SeqCst), 1);
            assert_eq!(host.secret("DRIVE"), Ok(Some("ACCESS-2".to_string())));
            assert_eq!(due.calls.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn the_credentials_host_refreshes_on_secret_of_through_its_transport() {
            use crate::secret_source::SecretSource;
            let (now, _, store) = connected();
            expire(&now);
            let host = crate::credentials::Credentials::default()
                .with_encrypted_store(store)
                .unwrap()
                .with_grant_transport(Arc::new(Scripted::new(vec![Scripted::ok(ROTATED)])));
            assert_eq!(host.secret_of("DRIVE", Some("u1")), Ok(Some("ACCESS-2".to_string())));
        }
    }
}
