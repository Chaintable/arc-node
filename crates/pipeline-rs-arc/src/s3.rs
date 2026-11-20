use aws_sdk_s3::{primitives::ByteStream, Client};

use crate::{ConditionalPut, ObjectSpec, ObjectStore};

/// S3 transport. Configure endpoint, credentials and retry settings on the client.
pub struct S3ObjectStore(pub Client);

#[derive(Debug, thiserror::Error)]
#[error("S3 {operation} failed for {bucket}/{key}: {source}")]
pub struct S3Error {
    operation: &'static str,
    bucket: String,
    key: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl S3Error {
    fn new(
        operation: &'static str,
        object: &ObjectSpec<'_>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            operation,
            bucket: object.bucket.to_owned(),
            key: object.key.clone(),
            source: Box::new(source),
        }
    }
}

impl ObjectStore for S3ObjectStore {
    type Error = S3Error;

    async fn create_if_absent(
        &self,
        object: &ObjectSpec<'_>,
        data: &[u8],
    ) -> Result<ConditionalPut, S3Error> {
        match self
            .0
            .put_object()
            .bucket(object.bucket)
            .key(&object.key)
            .if_none_match("*")
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await
        {
            Ok(_) => Ok(ConditionalPut::Created),
            Err(error) => match error.raw_response().map(|r| r.status().as_u16()) {
                Some(412) => Ok(ConditionalPut::AlreadyExists),
                Some(409) => Ok(ConditionalPut::ConcurrentWrite),
                _ => Err(S3Error::new("PutObject", object, error)),
            },
        }
    }

    async fn read(&self, object: &ObjectSpec<'_>) -> Result<Vec<u8>, S3Error> {
        let response = self
            .0
            .get_object()
            .bucket(object.bucket)
            .key(&object.key)
            .send()
            .await
            .map_err(|error| S3Error::new("GetObject", object, error))?;
        let body = response
            .body
            .collect()
            .await
            .map_err(|error| S3Error::new("GetObject body", object, error))?;
        Ok(body.into_bytes().to_vec())
    }
}
