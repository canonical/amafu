pub mod change;
pub mod kinds;
pub mod name;
pub mod object;
pub mod object_id;
pub mod registry;
pub mod resource;
pub mod validation;

pub use change::{Change, ChangeError, apply_change, confirm_removal};
pub use name::{InvalidName, NodeName, ObjectName, ProjectName};
pub use object::{Metadata, Object, ObjectReference};
pub use object_id::ObjectId;
pub use resource::{Resource, Scope};
pub use validation::{Validate, ValidationError, Violation};
