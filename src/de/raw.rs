use std::{convert::TryInto, io::Read};

use serde::{forward_to_deserialize_any};
use serde_json::json;

use crate::{oid::ObjectId, spec::BinarySubtype, spec::ElementType, Binary};

use super::{read_cstring, read_f64, read_i32, read_u8, Error};
use super::{read_i64, read_string, Result};
use crate::de::serde::MapDeserializer;

// hello

struct CountReader<R> {
    reader: R,
    bytes_read: usize,
}

impl<R: Read> CountReader<R> {
    /// Constructs a new CountReader that wraps `reader`.
    pub(super) fn new(reader: R) -> Self {
        CountReader {
            reader,
            bytes_read: 0,
        }
    }

    /// Gets the number of bytes read so far.
    pub(super) fn bytes_read(&self) -> usize {
        self.bytes_read
    }
}

impl<R: Read> Read for CountReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.reader.read(buf)?;
        self.bytes_read += bytes;
        Ok(bytes)
    }
}

pub(crate) struct Deserializer<R> {
    reader: CountReader<R>,
    current_type: ElementType,
}

impl<R> Deserializer<R>
where
    R: Read,
{
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader: CountReader::new(reader),
            current_type: ElementType::EmbeddedDocument,
        }
    }
}

impl<'de, 'a, R: Read> serde::de::Deserializer<'de> for &'a mut Deserializer<R> {
    type Error = Error;

    fn deserialize_any<V>(mut self, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.current_type {
            ElementType::Int32 => visitor.visit_i32(read_i32(&mut self.reader)?),
            ElementType::Int64 => visitor.visit_i64(read_i64(&mut self.reader)?),
            ElementType::Double => visitor.visit_f64(read_f64(&mut self.reader)?),
            ElementType::String => visitor.visit_string(read_string(&mut self.reader, true)?),
            ElementType::Boolean => visitor.visit_bool(read_u8(&mut self.reader)? == 1),
            ElementType::Null => visitor.visit_none(),
            ElementType::ObjectId => {
                let oid = ObjectId::from_reader(&mut self.reader)?;
                visitor.visit_map(ObjectIdAccess::new(oid))
            }
            ElementType::EmbeddedDocument => {
                let length = read_i32(&mut self.reader)?;
                visitor.visit_map(MapAccess {
                    root_deserializer: &mut self,
                    length_remaining: length - 4,
                })
            }
            ElementType::Array => {
                let length = read_i32(&mut self.reader)?;
                visitor.visit_seq(ArrayAccess {
                    root_deserializer: &mut self,
                    length_remaining: length - 4,
                })
            }
            ElementType::Binary => {
                let length = read_i32(&mut self.reader)?;
                let subtype = BinarySubtype::from(read_u8(&mut self.reader)?);

                // TODO: handle error here
                let ulength: usize = length.try_into().unwrap();
                let mut bytes = vec![0u8; ulength];
                self.reader.read_exact(&mut bytes)?;
                match subtype {
                    BinarySubtype::Generic => visitor.visit_byte_buf(bytes),
                    _ => {
                        let mut d = BD {
                            binary: Binary { subtype, bytes },
                            stage: BinaryDeserializationStage::TopLevel,
                        };
                        visitor.visit_map(BinaryAccess {
                            deserializer: &mut d,
                        })
                    }
                }
            }
            ElementType::Undefined => {
                let doc = doc! { "$undefined": 1 };
                let len = doc.len();
                visitor.visit_map(MapDeserializer {
                    iter: doc.into_iter(),
                    value: None,
                    len
                })
            }
            ElementType::DateTime => {

            }
            // ElementType::RegularExpression => {}
            // ElementType::DbPointer => {}
            // ElementType::JavaScriptCode => {}
            // ElementType::Symbol => {}
            // ElementType::JavaScriptCodeWithScope => {}
            // ElementType::Timestamp => {}
            // ElementType::Decimal128 => {}
            // ElementType::MaxKey => {}
            // ElementType::MinKey => {}
            _ => todo!(),
        }
    }

    forward_to_deserialize_any! {
        bool char str bytes byte_buf option unit unit_struct string
            newtype_struct seq tuple tuple_struct struct map enum
            ignored_any i8 i16 i32 i64 u8 u16 u32 u64 f32 f64
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        let s = read_cstring(&mut self.reader)?;
        visitor.visit_string(s)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

struct MapAccess<'d, T: 'd> {
    root_deserializer: &'d mut Deserializer<T>,
    length_remaining: i32,
}

impl<'de, 'd, R: Read> serde::de::MapAccess<'de> for MapAccess<'d, R> {
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        let tag = read_u8(&mut self.root_deserializer.reader)?;
        self.length_remaining -= 1;
        if tag == 0 {
            if self.length_remaining != 0 {
                panic!(
                    "got null byte but still have length {} remaining",
                    self.length_remaining
                )
            }
            return Ok(None);
        }
        // TODO: handle bad tags
        self.root_deserializer.current_type = ElementType::from(tag).unwrap();
        let start_bytes = self.root_deserializer.reader.bytes_read();
        let out = seed
            .deserialize(DocumentKeyDeserializer {
                root_deserializer: &mut *self.root_deserializer,
            })
            .map(Some);
        let bytes_read = self.root_deserializer.reader.bytes_read() - start_bytes;
        self.length_remaining -= bytes_read as i32;

        if self.length_remaining <= 0 {
            panic!("ran out of bytes!");
        }
        out
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        let start_bytes = self.root_deserializer.reader.bytes_read();
        let out = seed.deserialize(&mut *self.root_deserializer);
        let bytes_read = self.root_deserializer.reader.bytes_read() - start_bytes;
        self.length_remaining -= bytes_read as i32;

        if self.length_remaining <= 0 {
            panic!("ran out of bytes!");
        }
        out
    }
}

struct DocumentKeyDeserializer<'d, R> {
    root_deserializer: &'d mut Deserializer<R>,
}

impl<'de, 'a, R: Read> serde::de::Deserializer<'de> for DocumentKeyDeserializer<'a, R> {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        let s = read_cstring(&mut self.root_deserializer.reader)?;
        visitor.visit_string(s)
    }

    forward_to_deserialize_any! {
        bool char str bytes byte_buf option unit unit_struct string
        identifier newtype_struct seq tuple tuple_struct struct map enum
        ignored_any i8 i16 i32 i64 u8 u16 u32 u64 f32 f64
    }
}

struct ArrayAccess<'d, T: 'd> {
    root_deserializer: &'d mut Deserializer<T>,
    length_remaining: i32,
}

impl<'d, 'de, T: Read + 'd> serde::de::SeqAccess<'de> for ArrayAccess<'d, T> {
    type Error = Error;

    fn next_element_seed<S>(&mut self, seed: S) -> Result<Option<S::Value>>
    where
        S: serde::de::DeserializeSeed<'de>
    {
        let tag = read_u8(&mut self.root_deserializer.reader)?;
        self.length_remaining -= 1;
        if tag == 0 {
            if self.length_remaining != 0 {
                panic!(
                    "got null byte but still have length {} remaining",
                    self.length_remaining
                )
            }
            return Ok(None);
        }
        // TODO: handle bad tags
        self.root_deserializer.current_type = ElementType::from(tag).unwrap();
        let start_bytes = self.root_deserializer.reader.bytes_read();
        let _index = read_cstring(&mut self.root_deserializer.reader)?;
        let bytes_read = self.root_deserializer.reader.bytes_read() - start_bytes;
        self.length_remaining -= bytes_read as i32;

        if self.length_remaining <= 0 {
            panic!("ran out of bytes!");
        }

        let start_bytes = self.root_deserializer.reader.bytes_read();
        let out = seed.deserialize(&mut *self.root_deserializer);
        let bytes_read = self.root_deserializer.reader.bytes_read() - start_bytes;
        self.length_remaining -= bytes_read as i32;

        if self.length_remaining <= 0 {
            panic!("ran out of bytes!");
        }
        out.map(Some)
    }
}

struct FieldDeserializer {
    field_name: &'static str,
}

impl<'de> serde::de::Deserializer<'de> for FieldDeserializer {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        // visitor.visit_borrowed_str(self.field_name)
        visitor.visit_string(self.field_name.to_string())
    }

    serde::forward_to_deserialize_any! {
        bool u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 char str string seq
        bytes byte_buf map struct option unit newtype_struct
        ignored_any unit_struct tuple_struct tuple enum identifier
    }
}

struct ObjectIdAccess {
    oid: ObjectId,
    visited: bool,
}

impl ObjectIdAccess {
    fn new(oid: ObjectId) -> Self {
        Self {
            oid,
            visited: false,
        }
    }
}

impl<'de> serde::de::MapAccess<'de> for ObjectIdAccess {
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        if self.visited {
            return Ok(None);
        }
        self.visited = true;
        seed.deserialize(FieldDeserializer { field_name: "$oid" })
            .map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        seed.deserialize(ObjectIdDeserializer(self.oid))
    }
}

struct ObjectIdDeserializer(ObjectId);

impl<'de> serde::de::Deserializer<'de> for ObjectIdDeserializer {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_string(self.0.to_hex())
    }

    serde::forward_to_deserialize_any! {
        bool u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 char str string seq
        bytes byte_buf map struct option unit newtype_struct
        ignored_any unit_struct tuple_struct tuple enum identifier
    }
}

// struct FieldAccess<T> {
//     deserializer: T,
//     visited: bool,
//     field_name: &'static str,
// }

// impl<'de, T: serde::de::MapAccess<'de, Error=Error>> serde::de::MapAccess<'de> for FieldAccess<T> {
//     type Error = Error;

//     fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
//     where
//         K: serde::de::DeserializeSeed<'de>,
//     {
//         if self.visited {
//             return Ok(None);
//         }
//         self.visited = true;
//         seed.deserialize(FieldDeserializer { field_name: self.field_name })
//             .map(Some)
//     }

//     fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
//     where
//         V: serde::de::DeserializeSeed<'de>,
//     {
//         seed.deserialize(FieldDeserializer { access: self.deserializer })
//     }
// }

struct BinaryAccess<'d> {
    deserializer: &'d mut BD,
}

impl<'de, 'd> serde::de::MapAccess<'de> for BinaryAccess<'d> {
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        match self.deserializer.stage {
            BinaryDeserializationStage::TopLevel => seed
                .deserialize(FieldDeserializer {
                    field_name: "$binary",
                })
                .map(Some),
            BinaryDeserializationStage::Subtype => seed
                .deserialize(FieldDeserializer {
                    field_name: "subType",
                })
                .map(Some),
            BinaryDeserializationStage::Bytes => seed
                .deserialize(FieldDeserializer {
                    field_name: "base64",
                })
                .map(Some),
            BinaryDeserializationStage::Done => Ok(None),
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        seed.deserialize(&mut *self.deserializer)
    }
}

struct FieldValueDeserializer<T> {
    access: T,
}

// impl<'de, T: serde::de::MapAccess<'de>> serde::de::Deserializer<'de> for FieldValueDeserializer<D> {
//     type Error = Error;

//     fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
//     where
//         V: serde::de::Visitor<'de> {
//         visitor.visit_map(self.access)
//     }

//     serde::forward_to_deserialize_any! {
//         bool u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 char str string seq
//         bytes byte_buf map struct option unit newtype_struct
//         ignored_any unit_struct tuple_struct tuple enum identifier
//     }
// }

struct BD {
    binary: Binary,
    stage: BinaryDeserializationStage,
}

impl<'de, 'a> serde::de::Deserializer<'de> for &'a mut BD {
    type Error = Error;

    fn deserialize_any<V>(mut self, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.stage {
            BinaryDeserializationStage::TopLevel => {
                self.stage = BinaryDeserializationStage::Subtype;
                visitor.visit_map(BinaryAccess {
                    deserializer: &mut self,
                })
            }
            BinaryDeserializationStage::Subtype => {
                self.stage = BinaryDeserializationStage::Bytes;
                visitor.visit_string(hex::encode([u8::from(self.binary.subtype)]))
                // visitor.visit_u8(self.binary.subtype.into())
            }
            BinaryDeserializationStage::Bytes => {
                self.stage = BinaryDeserializationStage::Done;
                visitor.visit_string(base64::encode(self.binary.bytes.as_slice()))
                // visitor.visit_bytes(self.binary.bytes.as_slice())
                // println!("visiting binary");
                // visitor.visit_bytes(self.binary.bytes.as_slice())
            }
            BinaryDeserializationStage::Done => todo!(),
        }
    }

    serde::forward_to_deserialize_any! {
        bool u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 char str string seq
        bytes byte_buf map struct option unit newtype_struct
        ignored_any unit_struct tuple_struct tuple enum identifier
    }
}

// struct BinaryDeserializer<'b> {
//     binary: &'b Binary,
//     stage: &'b mut BinaryDeserializationStage
// }

// impl<'de, 'b> serde::de::Deserializer<'de> for BinaryDeserializer<'b> {
//     type Error = Error;

//     fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
//     where
//         V: serde::de::Visitor<'de>,
//     {
//         // match self.stage {
//         //     BinaryDeserializationStage::Subtype => visitor.visit_u8(self.binary.subtype.into()),
//         //     BinaryDeserializationStage::Bytes => visitor.visit_bytes(self.binary.bytes.as_slice()),
//         //     BinaryDeserializationStage::Done => todo!(),
//         // }
//         match self.stage {
//             BinaryDeserializationStage::TopLevel => {
//                 self.stage = BinaryDeserializationStage::Subtype;
//                 visitor.visit_map(todo!())
//             }
//             BinaryDeserializationStage::Subtype => {
//                 *self.stage = BinaryDeserializationStage::Bytes;
//                 visitor.visit_string(hex::encode([u8::from(self.binary.subtype)]))
//             }
//             BinaryDeserializationStage::Bytes => {
//                 *self.stage = BinaryDeserializationStage::Done;
//                 visitor.visit_string(base64::encode(self.binary.bytes.as_slice()))
//             }
//             BinaryDeserializationStage::Done => todo!(),
//         }
//     }

//     serde::forward_to_deserialize_any! {
//         bool u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 char str string seq
//         bytes byte_buf map struct option unit newtype_struct
//         ignored_any unit_struct tuple_struct tuple enum identifier
//     }
// }

enum BinaryDeserializationStage {
    TopLevel,
    Subtype,
    Bytes,
    Done,
}

// struct BinaryMapAccess {
//     stage: BinaryDeserializationStage,
//     binary: Binary,
// }

// impl BinaryMapAccess {
//     fn new(binary: Binary) -> Self {
//         BinaryMapAccess {
//             stage: BinaryDeserializationStage::Subtype,
//             binary
//         }
//     }
// }

// impl<'de> serde::de::MapAccess<'de> for BinaryMapAccess {
//     type Error = Error;

//     fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
//     where
//         K: serde::de::DeserializeSeed<'de>,
//     {
//         let key = match self.stage {
//             BinaryDeserializationStage::Subtype => {
//                 // self.stage = BinaryDeserializationStage::Bytes;
//                 "subtype"
//             },
//             BinaryDeserializationStage::Bytes => {
//                 // self.stage = BinaryDeserializationStage::Done;
//                 "bytes"
//             },
//             BinaryDeserializationStage::Done => return Ok(None)
//         };
//         seed.deserialize(FieldDeserializer { field_name: key })
//             .map(Some)
//     }

//     fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
//     where
//         V: serde::de::DeserializeSeed<'de>,
//     {
//         seed.deserialize(BinaryDeserializer { binary: &self.binary, stage: &mut self.stage })
//     }
// }

#[cfg(test)]
mod test {
    use std::time::Instant;

    use serde::Deserialize;

    use crate::{Binary, Bson, Document, oid::ObjectId};
    use crate::tests::LOCK;

    use super::Deserializer;

    #[derive(Debug, Deserialize)]
    struct D {
        x: i32,
        y: i32,
        i: I,
        // oid: ObjectId,
        null: Option<i32>,
        b: bool,
        d: f32,
    }

    #[derive(Debug, Deserialize)]
    struct I {
        a: i32,
        b: i32,
    }

    #[derive(Debug, Deserialize)]
    struct B {
        ok: i32,
        x: i32,
        y: i32,
        i: Ii,
        array: Vec<Bson>,
        oid: ObjectId,
        null: Option<()>,
        b: bool,
        d: f64,
        binary: Binary,
    }

    #[derive(Deserialize, Debug)]
    struct Ii {
        a: i32,
        b: i32,
    }

    #[test]
    fn raw() {
        let _guard = LOCK.run_concurrently();

        let doc = doc! {
            "ok": 1,
            "x": 1,
            "y": 2,
            "i": { "a": 300, "b": 12345 },
            "array": [ true, "oke", { "12": 24 } ],
            "oid": ObjectId::new(),
            "null": crate::Bson::Null,
            "b": true,
            "d": 12.5,
            "binary": crate::Binary { bytes: vec![36, 36, 36], subtype: crate::spec::BinarySubtype::Generic }
        };
        let mut bson = vec![0u8; 0];
        doc.to_writer(&mut bson).unwrap();

        // let mut de = Deserializer::new(bson.as_slice());
        // // let cr = CommandResponse::deserialize(&mut de).unwrap();
        // let t = B::deserialize(&mut de).unwrap();
        // println!("doc: {:?}", t);

        // let d = Document::from_reader(bson.as_slice()).unwrap();
        // let t: Document = crate::from_document(d).unwrap();

        // let j: serde_json::Value = crate::from_document(doc.clone()).unwrap();
        // let j = serde_json::to_value(doc.clone()).unwrap();
        // println!("{:?}", j);

        let raw_start = Instant::now();
        for i in 0..10_000 {
            let mut de = Deserializer::new(bson.as_slice());
            // let cr = CommandResponse::deserialize(&mut de).unwrap();
            // let t = Document::deserialize(&mut de).unwrap();
            let t = B::deserialize(&mut de).unwrap();

            if i == 0 {
                println!("raw: {:#?}", t);
            }
        }
        let raw_time = raw_start.elapsed();
        println!("raw time: {}", raw_time.as_secs_f32());

        let normal_start = Instant::now();
        for i in 0..10_000 {
            // let mut de = Deserializer::new(bson.as_slice());
            // // let cr = CommandResponse::deserialize(&mut de).unwrap();
            // let t = D::deserialize(&mut de).unwrap();
            let d = Document::from_reader(bson.as_slice()).unwrap();
            // let t: Document = crate::from_document(d).unwrap();
            let t: B = crate::from_document(d).unwrap();
            if i == 0 {
                println!("normal: {:#?}", t);
            }
        }
        let normal_time = normal_start.elapsed();
        println!("normal time: {}", normal_time.as_secs_f32());

        let normal_start = Instant::now();
        for _ in 0..10_000 {
            // let mut de = Deserializer::new(bson.as_slice());
            // // let cr = CommandResponse::deserialize(&mut de).unwrap();
            // let t = D::deserialize(&mut de).unwrap();
            // let d = Document::from_reader(bson.as_slice()).unwrap();
            // let t: Document = crate::from_document(doc.clone()).unwrap();
            let _d = Document::from_reader(bson.as_slice()).unwrap();
        }
        let normal_time = normal_start.elapsed();
        println!("decode time: {}", normal_time.as_secs_f32());
    }
}
