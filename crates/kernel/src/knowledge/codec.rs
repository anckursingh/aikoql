//! Deterministic canonical binary codec for KOM types.
//!
//! Guarantees (conformance-tested):
//! - round-trip: decode(encode(x)) == x for all KO/KE values
//! - canonical: equal values encode to identical bytes (BTreeMap ordering)
//! - strict: decoding rejects truncated input and trailing garbage
//! - extension preservation: unknown extension fields survive (MRFC-0001 req 9)
//!
//! Format v1 (big-endian, length-prefixed). Will be superseded by the MRFC-0005
//! binary format (prost/rkyv) without changing KS-ABI semantics.

use crate::knowledge::kom::*;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Enc {
    pub buf: Vec<u8>,
}

impl Enc {
    pub fn new() -> Self {
        Enc { buf: Vec::new() }
    }
    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.u8(if v { 1 } else { 0 });
    }
    pub fn u32(&mut self, v: u32) {
        self.raw(&v.to_be_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.raw(&v.to_be_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.raw(&v.to_be_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.raw(&v.to_be_bytes());
    }
    pub fn f64(&mut self, v: f64) {
        self.raw(&v.to_be_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.raw(b);
    }
    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    pub fn opt_str(&mut self, s: Option<&str>) {
        match s {
            None => self.u8(0),
            Some(v) => {
                self.u8(1);
                self.str(v);
            }
        }
    }
    pub fn hash256(&mut self, h: &[u8; 32]) {
        self.raw(h);
    }
    pub fn opt_hash256(&mut self, h: Option<&[u8; 32]>) {
        match h {
            None => self.u8(0),
            Some(v) => {
                self.u8(1);
                self.hash256(v);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decoder (strict: bounds-checked, rejects trailing bytes via `finish`)
// ---------------------------------------------------------------------------

pub struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Dec { buf, pos: 0 }
    }

    fn take(&mut self, n: usize, what: &str) -> KResult<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(KError::Codec(format!("truncated {}", what)));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn finish(self) -> KResult<()> {
        if self.pos != self.buf.len() {
            return Err(KError::Codec(format!(
                "trailing {} bytes",
                self.buf.len() - self.pos
            )));
        }
        Ok(())
    }

    pub fn raw(&mut self, n: usize) -> KResult<&'a [u8]> {
        self.take(n, "bytes")
    }
    pub fn u8(&mut self) -> KResult<u8> {
        Ok(self.take(1, "u8")?[0])
    }
    pub fn bool(&mut self) -> KResult<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            t => Err(KError::Codec(format!("invalid bool tag {}", t))),
        }
    }
    pub fn u32(&mut self) -> KResult<u32> {
        let b = self.take(4, "u32")?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> KResult<u64> {
        let b = self.take(8, "u64")?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    pub fn i64(&mut self) -> KResult<i64> {
        let b = self.take(8, "i64")?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    pub fn f32(&mut self) -> KResult<f32> {
        let b = self.take(4, "f32")?;
        Ok(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn f64(&mut self) -> KResult<f64> {
        let b = self.take(8, "f64")?;
        Ok(f64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    pub fn bytes(&mut self) -> KResult<Vec<u8>> {
        let n = self.u64()? as usize;
        Ok(self.take(n, "byte string")?.to_vec())
    }
    pub fn str(&mut self) -> KResult<String> {
        let b = self.bytes()?;
        String::from_utf8(b).map_err(|_| KError::Codec("invalid utf-8".into()))
    }
    pub fn opt_str(&mut self) -> KResult<Option<String>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.str()?)),
            t => Err(KError::Codec(format!("invalid option tag {}", t))),
        }
    }
    pub fn opt_hash256(&mut self) -> KResult<Option<[u8; 32]>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.hash256()?)),
            t => Err(KError::Codec(format!("invalid option tag {}", t))),
        }
    }
    pub fn hash256(&mut self) -> KResult<[u8; 32]> {
        let b = self.take(32, "hash256")?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }
    pub fn koid(&mut self) -> KResult<KOID> {
        let b = self.take(KOID_LEN, "koid")?;
        let mut a = [0u8; KOID_LEN];
        a.copy_from_slice(b);
        Ok(KOID(a))
    }
}

// ---------------------------------------------------------------------------
// Value (recursive, tagged)
// ---------------------------------------------------------------------------

fn enc_value(e: &mut Enc, v: &Value) {
    match v {
        Value::Null => e.u8(0),
        Value::Bool(b) => {
            e.u8(1);
            e.bool(*b);
        }
        Value::Int(i) => {
            e.u8(2);
            e.i64(*i);
        }
        Value::Float(f) => {
            e.u8(3);
            e.f64(*f);
        }
        Value::Text(s) => {
            e.u8(4);
            e.str(s);
        }
        Value::Bytes(b) => {
            e.u8(5);
            e.bytes(b);
        }
        Value::List(xs) => {
            e.u8(6);
            e.u64(xs.len() as u64);
            for x in xs {
                enc_value(e, x);
            }
        }
        Value::Map(m) => {
            e.u8(7);
            e.u64(m.len() as u64);
            for (k, v) in m {
                e.str(k);
                enc_value(e, v);
            }
        }
    }
}

fn dec_value(d: &mut Dec) -> KResult<Value> {
    match d.u8()? {
        0 => Ok(Value::Null),
        1 => Ok(Value::Bool(d.bool()?)),
        2 => Ok(Value::Int(d.i64()?)),
        3 => Ok(Value::Float(d.f64()?)),
        4 => Ok(Value::Text(d.str()?)),
        5 => Ok(Value::Bytes(d.bytes()?)),
        6 => {
            let n = d.u64()? as usize;
            let mut xs = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                xs.push(dec_value(d)?);
            }
            Ok(Value::List(xs))
        }
        7 => {
            let n = d.u64()? as usize;
            let mut m = BTreeMap::new();
            for _ in 0..n {
                let k = d.str()?;
                let v = dec_value(d)?;
                m.insert(k, v);
            }
            Ok(Value::Map(m))
        }
        t => Err(KError::Codec(format!("invalid value tag {}", t))),
    }
}

fn enc_map(e: &mut Enc, m: &BTreeMap<String, Value>) {
    e.u64(m.len() as u64);
    for (k, v) in m {
        e.str(k);
        enc_value(e, v);
    }
}

fn dec_map(d: &mut Dec) -> KResult<BTreeMap<String, Value>> {
    let n = d.u64()? as usize;
    let mut m = BTreeMap::new();
    for _ in 0..n {
        let k = d.str()?;
        let v = dec_value(d)?;
        m.insert(k, v);
    }
    Ok(m)
}

// ---------------------------------------------------------------------------
// Small enum / struct helpers
// ---------------------------------------------------------------------------

fn dec_tag<T>(d: &mut Dec, f: impl Fn(u8) -> Option<T>, what: &str) -> KResult<T> {
    let t = d.u8()?;
    f(t).ok_or_else(|| KError::Codec(format!("invalid {} tag {}", what, t)))
}

fn enc_origin(e: &mut Enc, o: &Origin) {
    match o {
        Origin::Human => e.u8(0),
        Origin::Agent(id) => {
            e.u8(1);
            e.str(id);
        }
        Origin::SemanticEnrichment => e.u8(2),
        Origin::Reason => e.u8(3),
        Origin::System => e.u8(4),
    }
}

fn dec_origin(d: &mut Dec) -> KResult<Origin> {
    match d.u8()? {
        0 => Ok(Origin::Human),
        1 => Ok(Origin::Agent(d.str()?)),
        2 => Ok(Origin::SemanticEnrichment),
        3 => Ok(Origin::Reason),
        4 => Ok(Origin::System),
        t => Err(KError::Codec(format!("invalid origin tag {}", t))),
    }
}

fn enc_semantic(e: &mut Enc, s: &SemanticBlock) {
    e.opt_str(s.embedding_model.as_deref());
    match &s.embedding {
        None => e.u8(0),
        Some(v) => {
            e.u8(1);
            e.u64(v.len() as u64);
            for x in v {
                e.f32(*x);
            }
        }
    }
    match s.confidence {
        None => e.u8(0),
        Some(c) => {
            e.u8(1);
            e.f32(c);
        }
    }
    e.opt_str(s.source.as_deref());
    e.opt_str(s.summary.as_deref());
}

fn dec_semantic(d: &mut Dec) -> KResult<SemanticBlock> {
    let embedding_model = d.opt_str()?;
    let embedding = match d.u8()? {
        0 => None,
        1 => {
            let n = d.u64()? as usize;
            let mut v = Vec::with_capacity(n.min(1 << 20));
            for _ in 0..n {
                v.push(d.f32()?);
            }
            Some(v)
        }
        t => return Err(KError::Codec(format!("invalid embedding tag {}", t))),
    };
    let confidence = match d.u8()? {
        0 => None,
        1 => Some(d.f32()?),
        t => return Err(KError::Codec(format!("invalid confidence tag {}", t))),
    };
    let source = d.opt_str()?;
    let summary = d.opt_str()?;
    Ok(SemanticBlock {
        embedding_model,
        embedding,
        confidence,
        source,
        summary,
    })
}

// ---------------------------------------------------------------------------
// KnowledgeObject
// ---------------------------------------------------------------------------

pub fn encode_ko(ko: &KnowledgeObject) -> Vec<u8> {
    let mut e = Enc::new();
    e.raw(ko.koid.as_bytes());
    e.u64(ko.version);
    e.u64(ko.commit_ts);
    // metadata
    e.str(&ko.metadata.type_name);
    e.opt_str(ko.metadata.tenant.as_deref());
    e.u32(ko.metadata.schema_version);
    e.u64(ko.metadata.tags.len() as u64);
    for t in &ko.metadata.tags {
        e.str(t);
    }
    // properties
    enc_map(&mut e, &ko.properties);
    // semantic
    match &ko.semantic {
        None => e.u8(0),
        Some(s) => {
            e.u8(1);
            enc_semantic(&mut e, s);
        }
    }
    // relationships
    e.u64(ko.relationships.len() as u64);
    for r in &ko.relationships {
        e.str(&r.rel_type);
        e.raw(r.target.as_bytes());
        e.u8(r.direction.tag());
    }
    // event refs
    e.u64(ko.event_refs.len() as u64);
    for r in &ko.event_refs {
        e.u64(r.seq);
        e.u8(r.kind.tag());
        e.u64(r.commit_ts);
    }
    // security
    e.str(&ko.security.owner);
    e.u64(ko.security.acl.len() as u64);
    for a in &ko.security.acl {
        e.str(&a.principal);
        e.u8(a.action.tag());
        e.u8(a.effect.tag());
    }
    e.opt_str(ko.security.classification.as_deref());
    // lifecycle
    e.u8(ko.lifecycle.state.tag());
    enc_origin(&mut e, &ko.lifecycle.origin);
    // extensions
    enc_map(&mut e, &ko.extensions);
    e.buf
}

pub fn decode_ko(buf: &[u8]) -> KResult<KnowledgeObject> {
    let mut d = Dec::new(buf);
    let koid = d.koid()?;
    let version = d.u64()?;
    let commit_ts = d.u64()?;
    let metadata = Metadata {
        type_name: d.str()?,
        tenant: d.opt_str()?,
        schema_version: d.u32()?,
        tags: {
            let n = d.u64()? as usize;
            let mut v = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                v.push(d.str()?);
            }
            v
        },
    };
    let properties = dec_map(&mut d)?;
    let semantic = match d.u8()? {
        0 => None,
        1 => Some(dec_semantic(&mut d)?),
        t => return Err(KError::Codec(format!("invalid semantic tag {}", t))),
    };
    let relationships = {
        let n = d.u64()? as usize;
        let mut v = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            v.push(RelationshipRef {
                rel_type: d.str()?,
                target: d.koid()?,
                direction: dec_tag(&mut d, Direction::from_tag, "direction")?,
            });
        }
        v
    };
    let event_refs = {
        let n = d.u64()? as usize;
        let mut v = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            v.push(EventRef {
                seq: d.u64()?,
                kind: dec_tag(&mut d, EventKind::from_tag, "event kind")?,
                commit_ts: d.u64()?,
            });
        }
        v
    };
    let security = SecurityDescriptor {
        owner: d.str()?,
        acl: {
            let n = d.u64()? as usize;
            let mut v = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                v.push(AclEntry {
                    principal: d.str()?,
                    action: dec_tag(&mut d, Action::from_tag, "action")?,
                    effect: dec_tag(&mut d, Effect::from_tag, "effect")?,
                });
            }
            v
        },
        classification: d.opt_str()?,
    };
    let lifecycle = Lifecycle {
        state: dec_tag(&mut d, LifecycleState::from_tag, "lifecycle")?,
        origin: dec_origin(&mut d)?,
    };
    let extensions = dec_map(&mut d)?;
    d.finish()?;
    Ok(KnowledgeObject {
        koid,
        version,
        commit_ts,
        metadata,
        properties,
        semantic,
        relationships,
        event_refs,
        security,
        lifecycle,
        extensions,
    })
}

// ---------------------------------------------------------------------------
// KnowledgeEvent
// ---------------------------------------------------------------------------

pub fn encode_ke(ke: &KnowledgeEvent) -> Vec<u8> {
    let mut e = Enc::new();
    e.u64(ke.seq);
    e.raw(ke.koid.as_bytes());
    e.u64(ke.version);
    e.u8(ke.kind.tag());
    enc_origin(&mut e, &ke.origin);
    e.str(&ke.actor);
    e.u64(ke.commit_ts);
    e.hash256(&ke.payload_hash);
    e.hash256(&ke.prev_audit_hash);
    e.hash256(&ke.audit_hash);
    e.opt_hash256(ke.signature.as_ref());
    e.opt_str(ke.note.as_deref());
    e.buf
}

pub fn decode_ke(buf: &[u8]) -> KResult<KnowledgeEvent> {
    let mut d = Dec::new(buf);
    let ke = KnowledgeEvent {
        seq: d.u64()?,
        koid: d.koid()?,
        version: d.u64()?,
        kind: dec_tag(&mut d, EventKind::from_tag, "event kind")?,
        origin: dec_origin(&mut d)?,
        actor: d.str()?,
        commit_ts: d.u64()?,
        payload_hash: d.hash256()?,
        prev_audit_hash: d.hash256()?,
        audit_hash: d.hash256()?,
        signature: d.opt_hash256()?,
        note: d.opt_str()?,
    };
    d.finish()?;
    Ok(ke)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ko() -> KnowledgeObject {
        let mut properties = PropertyMap::new();
        properties.insert("name".into(), Value::Text("aikoql".into()));
        properties.insert("score".into(), Value::Float(0.99));
        properties.insert("rank".into(), Value::Int(-7));
        properties.insert(
            "nested".into(),
            Value::Map(BTreeMap::from([(
                "list".into(),
                Value::List(vec![
                    Value::Bool(true),
                    Value::Null,
                    Value::Bytes(vec![1, 2, 3]),
                ]),
            )])),
        );
        let mut extensions = ExtensionMap::new();
        extensions.insert("x-future-field".into(), Value::Text("preserve me".into()));
        KnowledgeObject {
            koid: IdGen::new(9).next(1234),
            version: 3,
            commit_ts: 0xdead_beef,
            metadata: Metadata {
                type_name: "fact".into(),
                tenant: Some("acme".into()),
                schema_version: 2,
                tags: vec!["ai".into(), "memory".into()],
            },
            properties,
            semantic: Some(SemanticBlock {
                embedding_model: Some("bge-m3".into()),
                embedding: Some(vec![0.1, -2.5, 3.75]),
                confidence: Some(0.98),
                source: Some("sec-filing".into()),
                summary: Some("Revenue grew 10%".into()),
            }),
            relationships: vec![
                RelationshipRef {
                    rel_type: "cites".into(),
                    target: KOID([7u8; KOID_LEN]),
                    direction: Direction::Outbound,
                },
                RelationshipRef {
                    rel_type: "derived-from".into(),
                    target: KOID([8u8; KOID_LEN]),
                    direction: Direction::Inbound,
                },
            ],
            event_refs: vec![EventRef {
                seq: 42,
                kind: EventKind::Updated,
                commit_ts: 0xdead_beef,
            }],
            security: SecurityDescriptor {
                owner: "alice".into(),
                acl: vec![
                    AclEntry {
                        principal: "bob".into(),
                        action: Action::Read,
                        effect: Effect::Allow,
                    },
                    AclEntry {
                        principal: "contractors".into(),
                        action: Action::Write,
                        effect: Effect::Deny,
                    },
                ],
                classification: Some("internal".into()),
            },
            lifecycle: Lifecycle {
                state: LifecycleState::Verified,
                origin: Origin::Agent("agent-007".into()),
            },
            extensions,
        }
    }

    #[test]
    fn ko_round_trip_full() {
        let ko = sample_ko();
        let bytes = encode_ko(&ko);
        let back = decode_ko(&bytes).expect("decode");
        assert_eq!(ko, back);
    }

    #[test]
    fn ko_encoding_is_canonical() {
        let ko = sample_ko();
        assert_eq!(encode_ko(&ko), encode_ko(&ko));
    }

    #[test]
    fn ko_decode_rejects_truncation_and_trailing() {
        let ko = sample_ko();
        let bytes = encode_ko(&ko);
        assert!(matches!(
            decode_ko(&bytes[..bytes.len() - 1]),
            Err(KError::Codec(_))
        ));
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(matches!(decode_ko(&longer), Err(KError::Codec(_))));
    }

    #[test]
    fn ke_round_trip_full() {
        let ke = KnowledgeEvent {
            seq: 11,
            koid: KOID([3u8; KOID_LEN]),
            version: 2,
            kind: EventKind::LifecycleChanged,
            origin: Origin::Reason,
            actor: "scheduler".into(),
            commit_ts: 555,
            payload_hash: [3u8; 32],
            prev_audit_hash: [2u8; 32],
            audit_hash: [1u8; 32],
            signature: Some([4u8; 32]),
            note: Some("active -> verified".into()),
        };
        let bytes = encode_ke(&ke);
        assert_eq!(ke, decode_ke(&bytes).expect("decode"));
    }

    #[test]
    fn minimal_ko_round_trip() {
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: "t".into(),
                tenant: None,
                schema_version: 0,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "o".into(),
                acl: vec![],
                classification: None,
            },
        );
        assert_eq!(ko, decode_ko(&encode_ko(&ko)).expect("decode"));
    }
}
