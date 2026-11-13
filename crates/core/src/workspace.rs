//! The Workspace: the directory the model works in, the Sandbox its shell commands run in, and the
//! worktree a Session can have of its own.

mod protected;
mod sandbox;
mod worktree;

pub use protected::is_protected;
#[cfg(any(test, feature = "test-support"))]
pub use sandbox::MockContainerRuntime;
pub use sandbox::{
    CacheVolume, ContainerRuntime, ContainerSpec, Containers, DockerCli, Failure, Inspected, Mask,
    RUNTIMES, ReadOnly, RuntimeError, Sandbox,
};
pub(crate) use worktree::main_checkout_of;
#[cfg(any(test, feature = "test-support"))]
pub use worktree::test_support;
pub use worktree::{
    Checkout, GitError, REPOSITORY_ENV, approved_commands_path, create_worktree, remove_worktree,
    rename_worktree_branch, unfinished_work, worktree_branch, worktree_path,
};
