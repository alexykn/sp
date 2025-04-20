// **File:** sapphire-core/src/dependency/mod.rs (New file)
pub mod dependency;
pub mod requirement;
pub mod resolver;

// Re-export key types for easier access
pub use dependency::{Dependency, DependencyExt, DependencyTag};
pub use requirement::Requirement;
pub use resolver::{
    DependencyResolver, ResolutionContext, ResolutionStatus, ResolvedDependency, ResolvedGraph,
};
