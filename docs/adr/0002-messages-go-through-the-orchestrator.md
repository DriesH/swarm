# Messages go through the orchestrator

Workers never message each other. They can only ask or report to the orchestrator, and only the orchestrator sends messages to workers. The harness enforces this. Free agent-to-agent talk was considered, but it makes a swarm hard to follow and hard to steer. A person can still type into any worker; Swarm tells the orchestrator when that happens.
