use orbital_core::vector_store::*;
use std::fs;

const M: &str = "baai/bge-base-en-v1.5";

fn rec(model: &str, text: &str, dims: usize, seed: f32) -> (Key, Quantized) {
    let v: Vec<f32> = (0..dims)
        .map(|i| ((i as f32 + seed) * 0.37).sin())
        .collect();
    (text_key(model, text), quantize(&v))
}

#[test]
fn quantize_matches_ts_math() {
    // max=0.5 -> scale 0.5; 0.5->127, -0.25->round(-63.5)=-63 (JS half-up), 0.1->25.4->25, 0->0
    let q = quantize(&[0.5, -0.25, 0.1, 0.0]);
    assert_eq!(q.scale, 0.5);
    assert_eq!(
        q.bytes.iter().map(|b| *b as i8).collect::<Vec<_>>(),
        vec![127, -63, 25, 0]
    );
    let d = q.decode();
    assert!((d[1] - (-63.0 * 0.5 / 127.0)).abs() < 1e-7);
    let z = quantize(&[0.0, 0.0]);
    assert_eq!(z.scale, 1.0);
    assert_eq!(z.bytes, vec![0, 0]);
    // scale rounded to 6 significant digits like toPrecision(6)
    let r = quantize(&[f32::from_bits(0x3dfc_d6ea), -0.01]);
    assert_eq!(r.scale, 0.123457f32);
}

#[test]
fn round_trip_lookup_and_size() {
    let dims = 8u16;
    let recs: Vec<_> = (0..20)
        .map(|i| rec(M, &format!("t{i}"), dims as usize, i as f32))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v.orbvec");
    VectorStore::insert_many(&p, M, dims, recs.clone()).unwrap();
    let hdr = 6 + 2 + 2 + M.len() + 2 + 4;
    assert_eq!(
        fs::metadata(&p).unwrap().len() as usize,
        hdr + 20 * (36 + dims as usize)
    );
    let s = VectorStore::open(&p).unwrap();
    assert_eq!((s.model(), s.dims(), s.len()), (M, dims, 20));
    for (k, q) in &recs {
        let got = s.get(k).unwrap();
        assert_eq!(got.scale, q.scale);
        assert_eq!(got.bytes, &q.bytes[..]);
    }
    assert!(s.get(&text_key(M, "absent")).is_none());
    assert!(s.get(&text_key("other", "t0")).is_none());
}

#[test]
fn missing_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let s = VectorStore::open(&dir.path().join("nope")).unwrap();
    assert!(s.is_empty());
    assert!(s.get(&text_key(M, "x")).is_none());
}

#[test]
fn merge_preserves_and_dedups() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v");
    let a = rec(M, "a", 4, 1.0);
    let b = rec(M, "b", 4, 2.0);
    let a2 = (a.0, quantize(&[1.0, 0.0, 0.0, 0.0]));
    VectorStore::insert_many(&p, M, 4, vec![a.clone(), b.clone()]).unwrap();
    VectorStore::insert_many(&p, M, 4, vec![a2.clone(), a2.clone()]).unwrap();
    let s = VectorStore::open(&p).unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s.get(&a.0).unwrap().to_owned(), a2.1);
    assert_eq!(s.get(&b.0).unwrap().to_owned(), b.1);
}

#[test]
fn sequential_stale_writers_lose_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v");
    let stale_view = VectorStore::open(&p).unwrap(); // both writers began from this empty view
    assert!(stale_view.is_empty());
    let (a, b) = (rec(M, "a", 4, 1.0), rec(M, "b", 4, 2.0));
    VectorStore::insert_many(&p, M, 4, vec![a.clone()]).unwrap();
    VectorStore::insert_many(&p, M, 4, vec![b.clone()]).unwrap();
    let s = VectorStore::open(&p).unwrap();
    assert!(s.get(&a.0).is_some() && s.get(&b.0).is_some());
}

#[test]
fn layering_precedence_and_subset() {
    let dir = tempfile::tempdir().unwrap();
    let k = text_key(M, "x");
    let pkg = VectorStore::from_records(
        M,
        2,
        vec![(k, quantize(&[1.0, 0.0])), rec(M, "only-pkg", 2, 1.0)],
    )
    .unwrap();
    let user = VectorStore::from_records(
        M,
        2,
        vec![(k, quantize(&[0.0, 1.0])), rec(M, "only-user", 2, 2.0)],
    )
    .unwrap();
    let l = Layered::new(vec![pkg.clone(), user.clone(), VectorStore::empty()]).unwrap();
    assert_eq!(l.get(&k).unwrap().to_owned(), quantize(&[1.0, 0.0]));
    assert!(l.get(&text_key(M, "only-user")).is_some());
    assert!(l.get(&text_key(M, "nope")).is_none());
    let sub = pkg.subset(&[k, text_key(M, "missing")]).unwrap();
    assert_eq!(sub.len(), 1);
    let p = dir.path().join("snap");
    sub.write(&p).unwrap();
    assert_eq!(VectorStore::open(&p).unwrap().len(), 1);
}

#[test]
fn model_and_dims_mismatch_refused() {
    let a = VectorStore::from_records(M, 2, vec![rec(M, "a", 2, 1.0)]).unwrap();
    let b =
        VectorStore::from_records("other/model", 2, vec![rec("other/model", "a", 2, 1.0)]).unwrap();
    let c = VectorStore::from_records(M, 3, vec![rec(M, "a", 3, 1.0)]).unwrap();
    let e = Layered::new(vec![a.clone(), b]).unwrap_err().to_string();
    assert!(
        e.contains("ORB_VECTOR_MODEL_MISMATCH") && e.contains(M) && e.contains("other/model"),
        "{e}"
    );
    assert!(matches!(
        Layered::new(vec![a, c]),
        Err(VectorStoreError::DimsMismatch { .. })
    ));
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v");
    VectorStore::insert_many(&p, M, 2, vec![rec(M, "a", 2, 1.0)]).unwrap();
    assert!(matches!(
        VectorStore::insert_many(&p, "other/model", 2, vec![rec("other/model", "b", 2, 1.0)]),
        Err(VectorStoreError::ModelMismatch { .. })
    ));
    assert!(matches!(
        VectorStore::insert_many(&p, M, 3, vec![rec(M, "b", 3, 1.0)]),
        Err(VectorStoreError::DimsMismatch { .. })
    ));
    assert!(matches!(
        VectorStore::insert_many(&p, M, 2, vec![rec(M, "b", 3, 1.0)]),
        Err(VectorStoreError::BadRecord { .. })
    ));
    assert_eq!(VectorStore::open(&p).unwrap().len(), 1);
}

#[test]
fn truncated_and_garbage_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v");
    VectorStore::insert_many(
        &p,
        M,
        4,
        (0..5)
            .map(|i| rec(M, &format!("t{i}"), 4, i as f32))
            .collect(),
    )
    .unwrap();
    let bytes = fs::read(&p).unwrap();
    for cut in [3, 10, bytes.len() - 1, bytes.len() - 40] {
        let e = VectorStore::from_bytes(bytes[..cut].to_vec(), "t")
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("ORB_VECTOR_"), "{cut}: {e}");
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(VectorStore::from_bytes(extra, "t").is_err());
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert!(matches!(
        VectorStore::from_bytes(bad, "t"),
        Err(VectorStoreError::BadMagic { .. })
    ));
    fs::write(&p, &bytes[..bytes.len() - 1]).unwrap();
    assert!(VectorStore::open(&p).is_err());
}

#[test]
fn cosine_i8_equals_float_cosine() {
    let a = quantize(&(0..64).map(|i| (i as f32 * 0.7).sin()).collect::<Vec<_>>());
    let b = quantize(
        &(0..64)
            .map(|i| (i as f32 * 0.3).cos() * 4.0)
            .collect::<Vec<_>>(),
    );
    let (da, db) = (a.decode(), b.decode());
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in da.iter().zip(&db) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let float = dot / (na.sqrt() * nb.sqrt());
    assert!(
        (cosine_i8(&a.bytes, &b.bytes) - float).abs() < 1e-6,
        "f32 decode tolerance"
    );
    // exact in f64 decode
    let f64dec = |q: &Quantized| {
        q.bytes
            .iter()
            .map(|b| (*b as i8) as f64 * q.scale as f64 / 127.0)
            .collect::<Vec<_>>()
    };
    let (xa, xb) = (f64dec(&a), f64dec(&b));
    let d: f64 = xa.iter().zip(&xb).map(|(x, y)| x * y).sum();
    let n =
        xa.iter().map(|x| x * x).sum::<f64>().sqrt() * xb.iter().map(|x| x * x).sum::<f64>().sqrt();
    assert!((cosine_i8(&a.bytes, &b.bytes) - d / n).abs() < 1e-9);
}

fn quick_lock() -> LockOptions {
    LockOptions {
        stale_after: std::time::Duration::from_secs(60),
        timeout: std::time::Duration::from_secs(20),
        poll: std::time::Duration::from_millis(2),
    }
}

#[test]
fn interleaved_writers_through_the_lock_lose_nothing() {
    let dims = 8u16;
    let dir = tempfile::tempdir().unwrap();
    let p = std::sync::Arc::new(dir.path().join("v.orbvec"));
    let writers = 6usize;
    let per = 12usize;
    let start = std::sync::Arc::new(std::sync::Barrier::new(writers));
    let handles: Vec<_> = (0..writers)
        .map(|w| {
            let (p, start) = (p.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                for i in 0..per {
                    let r = rec(
                        M,
                        &format!("w{w}-t{i}"),
                        dims as usize,
                        (w * 100 + i) as f32,
                    );
                    VectorStore::insert_many_with(&p, M, dims, vec![r], &quick_lock()).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let s = VectorStore::open(&p).unwrap();
    assert_eq!(s.len(), writers * per);
    for w in 0..writers {
        for i in 0..per {
            assert!(s.get(&text_key(M, &format!("w{w}-t{i}"))).is_some());
        }
    }
    assert!(!lock_path(&p).exists(), "the lock is released");
}

#[test]
fn a_held_lock_makes_the_writer_wait_and_merge_what_landed_meanwhile() {
    let dims = 8u16;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v.orbvec");
    let held = VectorStore::lock(&p, &quick_lock()).unwrap();
    let p2 = p.clone();
    let writer = std::thread::spawn(move || {
        let r = rec(M, "b", dims as usize, 2.0);
        VectorStore::insert_many_with(&p2, M, dims, vec![r], &quick_lock()).unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!writer.is_finished(), "the writer waits on the lock");
    VectorStore::from_records(M, dims, vec![rec(M, "a", dims as usize, 1.0)])
        .unwrap()
        .write(&p)
        .unwrap();
    drop(held);
    writer.join().unwrap();
    let s = VectorStore::open(&p).unwrap();
    assert!(s.get(&text_key(M, "a")).is_some());
    assert!(s.get(&text_key(M, "b")).is_some());
}

#[test]
fn a_stale_lock_is_broken() {
    let dims = 8u16;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v.orbvec");
    // A crashed writer: a lock naming a pid that is not running.
    fs::write(lock_path(&p), "4194999 0").unwrap();
    VectorStore::insert_many_with(
        &p,
        M,
        dims,
        vec![rec(M, "x", dims as usize, 1.0)],
        &quick_lock(),
    )
    .unwrap();
    assert!(VectorStore::open(&p)
        .unwrap()
        .get(&text_key(M, "x"))
        .is_some());
    // An aged lock whose pid is alive (this process) is stale by age.
    fs::write(lock_path(&p), format!("{} 0", std::process::id())).unwrap();
    let aged = LockOptions {
        stale_after: std::time::Duration::from_millis(0),
        ..quick_lock()
    };
    VectorStore::insert_many_with(&p, M, dims, vec![rec(M, "y", dims as usize, 2.0)], &aged)
        .unwrap();
    assert_eq!(VectorStore::open(&p).unwrap().len(), 2);
}

#[test]
fn a_live_fresh_lock_times_out_with_a_named_error() {
    let dims = 8u16;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("v.orbvec");
    let _held = VectorStore::lock(&p, &quick_lock()).unwrap();
    let short = LockOptions {
        timeout: std::time::Duration::from_millis(50),
        ..quick_lock()
    };
    let err =
        VectorStore::insert_many_with(&p, M, dims, vec![rec(M, "z", dims as usize, 1.0)], &short)
            .unwrap_err();
    assert!(err.to_string().starts_with("ORB_VECTOR_LOCKED"), "{err}");
}
