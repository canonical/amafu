pub mod object_storage;
pub mod signature;
pub mod syslog;

use std::sync::Arc;

use amafu_domain::kinds::audit_sink::AuditSinkSpecification;
use amafu_interfaces::{AuditSinkDelivery, AuditSinks};

use crate::object_storage::ObjectStorageSink;
use crate::syslog::SyslogSink;

#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltInAuditSinks;

impl AuditSinks for BuiltInAuditSinks {
    fn sink(&self, specification: &AuditSinkSpecification) -> Result<Arc<dyn AuditSinkDelivery>, String> {
        Ok(match specification {
            AuditSinkSpecification::Syslog { address, server_name, certificate_authority } => Arc::new(
                SyslogSink::new(address.clone(), server_name.clone(), certificate_authority.as_deref())?,
            ),
            AuditSinkSpecification::ObjectStorage {
                endpoint,
                bucket,
                region,
                prefix,
                access_key,
                secret_key,
            } => Arc::new(ObjectStorageSink::new(endpoint, bucket, region, prefix, access_key, secret_key)?),
        })
    }
}
