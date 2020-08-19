use super::{DrainIter, SingleIter, SingleIterFrom, SingleIterKeyMatch};
use crate::env::ReadManager;
use crate::next::{check_empty_key, kv::KvStore, BufKey, BufVal, BufferedStore};
use crate::{
    env::EnvironmentRead,
    error::{DatabaseError, DatabaseResult},
    prelude::{Readable, Writer},
};
use fallible_iterator::FallibleIterator;
use rkv::SingleStore;
use std::collections::BTreeMap;

type Scratch<V> = BTreeMap<Vec<u8>, KvOp<V>>;

/// Transactional operations on a KV store
/// Put: add or replace this KV
/// Delete: remove the KV
#[derive(Clone, Debug, PartialEq)]
pub enum KvOp<V> {
    Put(Box<V>),
    Delete,
}

/// A persisted key-value store with a transient HashMap to store
/// CRUD-like changes without opening a blocking read-write cursor
pub struct KvBufUsed<K, V>
where
    K: BufKey,
    V: BufVal,
{
    store: KvStore<K, V>,
    scratch: Scratch<V>,
    __phantom: std::marker::PhantomData<K>,
}

impl<'env, K, V> KvBufUsed<K, V>
where
    K: BufKey,
    V: BufVal,
{
    /// Constructor
    pub fn new(db: SingleStore) -> DatabaseResult<Self> {
        Ok(Self {
            store: KvStore::new(db),
            scratch: BTreeMap::new(),
            __phantom: std::marker::PhantomData,
        })
    }

    pub fn store(&self) -> &KvStore<K, V> {
        &self.store
    }

    /// See if a value exists, avoiding deserialization
    pub fn contains<R: Readable>(&self, r: &R, k: &K) -> DatabaseResult<bool> {
        check_empty_key(k)?;
        use KvOp::*;
        let exists = match self.scratch.get(k.as_ref()) {
            Some(Put(_)) => true,
            Some(Delete) => false,
            None => self.store.get(r, k)?.is_some(),
        };
        Ok(exists)
    }

    /// Get a value, taking the scratch space into account,
    /// or from persistence if needed
    pub fn get<R: Readable>(&self, r: &R, k: &K) -> DatabaseResult<Option<V>> {
        check_empty_key(k)?;
        use KvOp::*;
        let val = match self.scratch.get(k.as_ref()) {
            Some(Put(scratch_val)) => Some(*scratch_val.clone()),
            Some(Delete) => None,
            None => self.store.get(r, k)?,
        };
        Ok(val)
    }

    /// Update the scratch space to record a Put operation for the KV
    pub fn put(&mut self, k: K, v: V) -> DatabaseResult<()> {
        check_empty_key(&k)?;
        self.scratch.insert(k.into(), KvOp::Put(Box::new(v)));
        Ok(())
    }

    /// Update the scratch space to record a Delete operation for the KV
    pub fn delete(&mut self, k: K) -> DatabaseResult<()> {
        check_empty_key(&k)?;
        self.scratch.insert(k.into(), KvOp::Delete);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn scratch(&self) -> &Scratch<V> {
        &self.scratch
    }

    /// Iterator that checks the scratch space
    pub fn iter<'a, R: Readable>(&'a self, r: &'a R) -> DatabaseResult<SingleIter<'a, '_, V>> {
        Ok(SingleIter::new(
            &self.scratch,
            self.scratch.iter(),
            self.store.iter(r)?,
        ))
    }

    /// Iterator that tracks elements so they can be deleted
    pub fn drain_iter<'a, R: Readable>(
        &mut self,
        r: &'a R,
    ) -> DatabaseResult<DrainIter<'a, '_, V>> {
        Ok(DrainIter::new(&mut self.scratch, self.store.iter(r)?))
    }

    /// Iterator that returns all partial matches to this key
    pub fn iter_all_key_matches<'a, R: Readable>(
        &'a self,
        r: &'a R,
        k: K,
    ) -> DatabaseResult<SingleIterKeyMatch<V>> {
        check_empty_key(&k)?;

        let key = k.as_ref().to_vec();
        Ok(SingleIterKeyMatch::new(
            SingleIterFrom::new(&self.scratch, self.store.iter_from(r, k)?, key.clone()),
            key,
        ))
    }

    /// Iterate from a key onwards
    pub fn iter_from<'a, R: Readable>(
        &'a self,
        r: &'a R,
        k: K,
    ) -> DatabaseResult<SingleIterFrom<'a, '_, V>> {
        check_empty_key(&k)?;

        let key = k.as_ref().to_vec();
        Ok(SingleIterFrom::new(
            &self.scratch,
            self.store.iter_from(r, k)?,
            key,
        ))
    }

    /// Iterate over the data in reverse
    #[deprecated = "just use rev()"]
    pub fn iter_reverse<'a, R: Readable>(
        &'a self,
        r: &'a R,
    ) -> DatabaseResult<fallible_iterator::Rev<SingleIter<'a, '_, V>>> {
        Ok(self.iter(r)?.rev())
    }

    /// Iterator that tracks elements so they can be deleted but in reverse
    #[deprecated = "just use rev()"]
    pub fn drain_iter_reverse<'a, R: Readable>(
        &'a mut self,
        r: &'a R,
    ) -> DatabaseResult<fallible_iterator::Rev<DrainIter<'a, '_, V>>> {
        Ok(self.drain_iter(r)?.rev())
    }

    // TODO: This should be cfg test but can't because it's in a different crate
    /// Clear all scratch and db, useful for tests
    pub fn clear_all(&mut self, writer: &mut Writer) -> DatabaseResult<()> {
        self.scratch.clear();
        Ok(self.store.delete_all(writer)?)
    }
}

#[derive(shrinkwraprs::Shrinkwrap)]
pub struct KvBufFresh<K, V>
where
    K: BufKey,
    V: BufVal,
{
    env: EnvironmentRead,
    #[shrinkwrap(main_field)]
    inner: KvBufUsed<K, V>,
}

macro_rules! fresh_reader {
    ($self_: ident, $f: expr) => {{
        let g = $self_.env.guard().await;
        let r = g.reader()?;
        $f(r)
    }};
}

type IterOwned<V> = Vec<(Vec<u8>, V)>;

impl<'env, K, V> KvBufFresh<K, V>
where
    K: BufKey,
    V: BufVal,
{
    /// Create a new KvBufUsed from a read-only transaction and a database reference
    pub fn new(env: EnvironmentRead, db: SingleStore) -> DatabaseResult<Self> {
        Ok(Self {
            env,
            inner: KvBufUsed::new(db)?,
        })
    }

    /// See if a value exists, avoiding deserialization
    pub async fn contains(&self, k: &K) -> DatabaseResult<bool> {
        fresh_reader!(self, |reader| self.inner.contains(&reader, k))
    }

    /// Get a value, taking the scratch space into account,
    /// or from persistence if needed
    pub async fn get(&self, k: &K) -> DatabaseResult<Option<V>> {
        fresh_reader!(self, |reader| self.inner.get(&reader, k))
    }

    // /// Iterator that checks the scratch space
    // TODO: remove, not much point in collecting the entire DB, right?
    // pub async fn iter<'a, R: Readable + Send + Sync>(&'a self) -> DatabaseResult<IterOwned<V>> {
    //     fresh_reader!(self, |reader| Ok(self
    //         .inner
    //         .iter(&reader)?
    //         .map(|(k, v)| { Ok((k.to_vec(), v)) })
    //         .collect()?))
    // }

    /// Iterator that tracks elements so they can be deleted
    // NB: this cannot return an iterator due to lifetime issues
    pub async fn drain<R: Readable + Send + Sync>(&mut self) -> DatabaseResult<Vec<V>> {
        let g = self.env.guard().await;
        let r = g.reader()?;
        let v = self.inner.drain_iter(&r)?.collect()?;
        Ok(v)
    }

    /// Iterator that returns all partial matches to this key
    pub async fn iter_all_key_matches<'a, R: Readable + Send + Sync>(
        &'a self,
        k: K,
    ) -> DatabaseResult<IterOwned<V>> {
        fresh_reader!(self, |reader| Ok(self
            .inner
            .iter_all_key_matches(&reader, k)?
            .map(|(k, v)| { Ok((k.to_vec(), v)) })
            .collect()?))
    }

    // /// Iterate from a key onwards
    // TODO: remove, not much point in collecting the entire DB, right?
    // pub async fn iter_from<'a, R: Readable + Send + Sync>(
    //     &'a self,
    //     k: K,
    // ) -> DatabaseResult<SingleIterFrom<'a, '_, V>> {
    //     fresh_reader!(self, |reader| self.inner.iter_from(&reader, k))
    // }
}

impl<K, V> BufferedStore for KvBufUsed<K, V>
where
    K: BufKey,
    V: BufVal,
{
    type Error = DatabaseError;

    fn is_clean(&self) -> bool {
        self.scratch.is_empty()
    }

    fn flush_to_txn(self, writer: &mut Writer) -> DatabaseResult<()> {
        use KvOp::*;

        if self.is_clean() {
            return Ok(());
        }

        for (k, op) in self.scratch.iter() {
            match op {
                Put(v) => {
                    let buf = holochain_serialized_bytes::encode(v)?;
                    let encoded = rkv::Value::Blob(&buf);
                    self.store.db().put(writer, k, &encoded)?;
                }
                Delete => match self.store.db().delete(writer, k) {
                    Err(rkv::StoreError::LmdbError(rkv::LmdbError::NotFound)) => (),
                    r => r?,
                },
            }
        }

        Ok(())
    }
}

impl<K, V> BufferedStore for KvBufFresh<K, V>
where
    K: BufKey,
    V: BufVal,
{
    type Error = DatabaseError;

    fn is_clean(&self) -> bool {
        self.inner.is_clean()
    }

    fn flush_to_txn(self, writer: &mut Writer) -> DatabaseResult<()> {
        self.inner.flush_to_txn(writer)
    }
}

/////////////////////////////////