pub mod definition; // Renamed from 'dependency'
pub mod requirement;
pub mod resolver;

// Re-export key types for easier access
pub use definition::{Dependency, DependencyExt, DependencyTag}; // Updated source module
pub use requirement::Requirement;
pub use resolver::{
    DependencyResolver, ResolutionContext, ResolutionStatus, ResolvedDependency, ResolvedGraph,
};
