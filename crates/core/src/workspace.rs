//! The Workspace: the directory the model works in, the Sandbox its shell commands run in, and the
//! worktree a Session can have of its own.

mod sandbox;
mod worktree;

#[cfg(test)]
pub(crate) use sandbox::MockContainerRuntime;
pub use sandbox::{
    CacheVolume, ContainerRuntime, ContainerSpec, Containers, DockerCli, Failure, Inspected, Mask,
    RUNTIMES, ReadOnly, RuntimeError, Sandbox,
};
#[cfg(test)]
pub(crate) use worktree::test_support;
pub use worktree::{
    GitError, approved_commands_path, create_worktree, remove_worktree, rename_worktree_branch,
    unfinished_work, worktree_branch, worktree_path,
};
pub(crate) use worktree::{REPOSITORY_ENV, main_checkout_of};
