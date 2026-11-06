# Local single-user first release

The first release will manage existing local repository checkouts for one user on one machine. It will not include remote cloning, collaboration, multi-user accounts, remote control or execution, or custom code plugins; this keeps authorization, credential handling, process supervision, and recovery within a precise local trust boundary while the core workflow is validated.

## Consequences

- Every Project has one explicitly authorized local checkout.
- Provider access and analytics remain optional integrations rather than prerequisites for local work.
- Collaboration and remote execution require a future security and tenancy design instead of being inferred from the local model.
