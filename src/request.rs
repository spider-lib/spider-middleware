//! Data structures for representing HTTP requests in `spider-lib`.
//!
//! This module defines the `Request` struct, which is a central component
//! for constructing and managing outgoing HTTP requests within the
//! `spider-lib` framework. It encapsulates all necessary details of an
//! HTTP request, including:
//! - The target URL and HTTP method.
//! - Request headers and an optional request body (supporting JSON, form data, or raw bytes).
//! - Metadata for tracking retry attempts or other custom information.
//!
//! Additionally, the module provides methods for building requests,
//! incrementing retry counters, and generating unique fingerprints
//! for request deduplication and caching.

use bytes::Bytes;
use dashmap::DashMap;
use http::header::HeaderMap;
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use std::str::FromStr;

use spider_util::error::SpiderError;

#[derive(Debug, Clone)]
pub enum Body {
    Json(serde_json::Value),
    Form(DashMap<String, String>),
    Bytes(Bytes),
}

// Custom serialization for Body enum
impl Serialize for Body {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;

        match self {
            Body::Json(value) => map.serialize_entry("Json", value)?,
            Body::Form(dashmap) => {
                // Convert DashMap to HashMap for serialization
                let hmap: HashMap<String, String> = dashmap.clone().into_iter().collect();
                map.serialize_entry("Form", &hmap)?
            }
            Body::Bytes(bytes) => map.serialize_entry("Bytes", bytes)?,
        }

        map.end()
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct BodyVisitor;

        impl<'de> Visitor<'de> for BodyVisitor {
            type Value = Body;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a body object")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Body, V::Error>
            where
                V: MapAccess<'de>,
            {
                let entry = map.next_entry::<String, serde_json::Value>()?;
                let (key, value) = match entry {
                    Some((k, v)) => (k, v),
                    None => return Err(de::Error::custom("Expected a body variant")),
                };

                match key.as_str() {
                    "Json" => {
                        let json_value =
                            serde_json::from_value(value).map_err(de::Error::custom)?;
                        Ok(Body::Json(json_value))
                    }
                    "Form" => {
                        let form_data: HashMap<String, String> =
                            serde_json::from_value(value).map_err(de::Error::custom)?;
                        let dashmap = DashMap::new();
                        for (k, v) in form_data {
                            dashmap.insert(k, v);
                        }
                        Ok(Body::Form(dashmap))
                    }
                    "Bytes" => {
                        let bytes: Bytes =
                            serde_json::from_value(value).map_err(de::Error::custom)?;
                        Ok(Body::Bytes(bytes))
                    }
                    _ => Err(de::Error::custom(format!("Unknown body variant: {}", key))),
                }
            }
        }

        deserializer.deserialize_map(BodyVisitor)
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub url: Url,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Option<Body>,
    pub meta: DashMap<Cow<'static, str>, serde_json::Value>,
}

// Custom serialization for Request struct
impl Serialize for Request {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // Convert HeaderMap to a serializable format
        let headers_vec: Vec<(String, String)> = self
            .headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|val_str| (name.as_str().to_string(), val_str.to_string()))
            })
            .collect();

        let mut s = serializer.serialize_struct("Request", 5)?;
        s.serialize_field("url", &self.url.as_str())?;
        s.serialize_field("method", &self.method.as_str())?;
        s.serialize_field("headers", &headers_vec)?;
        s.serialize_field("body", &self.body)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Request {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Url,
            Method,
            Headers,
            Body,
        }

        struct RequestVisitor;

        impl<'de> Visitor<'de> for RequestVisitor {
            type Value = Request;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct Request")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Request, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut url = None;
                let mut method = None;
                let mut headers = None;
                let mut body = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Url => {
                            if url.is_some() {
                                return Err(de::Error::duplicate_field("url"));
                            }
                            let url_str: String = map.next_value()?;
                            let parsed_url = Url::parse(&url_str).map_err(de::Error::custom)?;
                            url = Some(parsed_url);
                        }
                        Field::Method => {
                            if method.is_some() {
                                return Err(de::Error::duplicate_field("method"));
                            }
                            let method_str: String = map.next_value()?;
                            let parsed_method =
                                Method::from_str(&method_str).map_err(de::Error::custom)?;
                            method = Some(parsed_method);
                        }
                        Field::Headers => {
                            if headers.is_some() {
                                return Err(de::Error::duplicate_field("headers"));
                            }
                            // Deserialize headers vector and convert back to HeaderMap
                            let headers_vec: Vec<(String, String)> = map.next_value()?;
                            let mut header_map = HeaderMap::new();
                            for (name, value) in headers_vec {
                                if let Ok(header_name) =
                                    http::header::HeaderName::from_bytes(name.as_bytes())
                                    && let Ok(header_value) =
                                        http::header::HeaderValue::from_str(&value)
                                {
                                    header_map.insert(header_name, header_value);
                                }
                            }
                            headers = Some(header_map);
                        }
                        Field::Body => {
                            if body.is_some() {
                                return Err(de::Error::duplicate_field("body"));
                            }
                            body = Some(map.next_value()?);
                        }
                    }
                }

                let url = url.ok_or_else(|| de::Error::missing_field("url"))?;
                let method = method.ok_or_else(|| de::Error::missing_field("method"))?;
                let headers = headers.ok_or_else(|| de::Error::missing_field("headers"))?;
                let body = body; // Optional field

                Ok(Request {
                    url,
                    method,
                    headers,
                    body,
                    meta: DashMap::new(), // Initialize empty meta map
                })
            }
        }

        const FIELDS: &[&str] = &["url", "method", "headers", "body"];
        deserializer.deserialize_struct("Request", FIELDS, RequestVisitor)
    }
}

impl Default for Request {
    fn default() -> Self {
        Self {
            url: Url::parse("http://default.invalid").unwrap(),
            method: Method::GET,
            headers: HeaderMap::new(),
            body: None,
            meta: DashMap::new(),
        }
    }
}

impl Request {
    /// Creates a new `Request` with the given URL.
    pub fn new(url: Url) -> Self {
        Request {
            url,
            method: Method::GET,
            headers: HeaderMap::new(),
            body: None,
            meta: DashMap::new(),
        }
    }

    /// Sets the HTTP method for the request.
    pub fn with_method(mut self, method: Method) -> Self {
        self.method = method;
        self
    }

    /// Adds a header to the request.
    pub fn with_header(mut self, name: &str, value: &str) -> Result<Self, SpiderError> {
        let header_name =
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                SpiderError::HeaderValueError(format!("Invalid header name '{}': {}", name, e))
            })?;
        let header_value = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
            SpiderError::HeaderValueError(format!("Invalid header value '{}': {}", value, e))
        })?;

        self.headers.insert(header_name, header_value);
        Ok(self)
    }

    /// Sets the body of the request and defaults the method to POST.
    pub fn with_body(mut self, body: Body) -> Self {
        self.body = Some(body);
        self.with_method(Method::POST)
    }

    /// Sets the body of the request to a JSON value.
    pub fn with_json(self, json: serde_json::Value) -> Self {
        self.with_body(Body::Json(json))
    }

    /// Sets the body of the request to a form.
    pub fn with_form(self, form: DashMap<String, String>) -> Self {
        self.with_body(Body::Form(form))
    }

    /// Sets the body of the request to a byte slice.
    pub fn with_bytes(self, bytes: Bytes) -> Self {
        self.with_body(Body::Bytes(bytes))
    }

    /// Adds a value to the request's metadata.
    pub fn with_meta(self, key: &str, value: serde_json::Value) -> Self {
        self.meta.insert(Cow::Owned(key.to_owned()), value);
        self
    }

    const RETRY_ATTEMPTS_KEY: &str = "retry_attempts";

    /// Gets the number of times the request has been retried.
    pub fn get_retry_attempts(&self) -> u32 {
        self.meta
            .get(Self::RETRY_ATTEMPTS_KEY)
            .and_then(|v| v.value().as_u64())
            .unwrap_or(0) as u32
    }

    /// Increments the retry count for the request.
    pub fn increment_retry_attempts(&mut self) {
        let current_attempts = self.get_retry_attempts();
        self.meta.insert(
            Cow::Borrowed(Self::RETRY_ATTEMPTS_KEY),
            serde_json::to_value(current_attempts + 1).unwrap(),
        );
    }

    /// Generates a unique fingerprint for the request based on its URL, method, and body.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.url.as_str().as_bytes());
        hasher.update(self.method.as_str().as_bytes());

        if let Some(ref body) = self.body {
            match body {
                Body::Json(json_val) => {
                    if let Ok(serialized) = serde_json::to_string(json_val) {
                        hasher.update(serialized.as_bytes());
                    }
                }
                Body::Form(form_val) => {
                    let mut form_string = String::new();
                    for r in form_val.iter() {
                        form_string.push_str(r.key());
                        form_string.push_str(r.value());
                    }
                    hasher.update(form_string.as_bytes());
                }
                Body::Bytes(bytes_val) => {
                    hasher.update(bytes_val);
                }
            }
        }
        hex::encode(hasher.finalize())
    }
}