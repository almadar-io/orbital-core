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
    };
    zeroize::Zeroize::zeroize(&mut stored.value);
    assert!(stored.value.is_empty(), "a zeroized String is emptied after its bytes are overwritten");
}
