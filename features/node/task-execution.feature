Feature: Task execution
  A node runs a task from a caller as one turn of its own agent. The node
  checks the caller first, so the agent never starts for work that the node
  must refuse.

  Scenario: An admitted task runs one agent turn
    Given a proof node that grants review to the caller
    When the caller sends a review task and waits for it to end
    Then the task succeeds with the result "review:untrusted task text"
    And the agent ran 1 turn
    And the task is succeeded
    And the task events are "queued, leased, running, result, succeeded"

  Scenario: Sending the same task again does not run it again
    Given a proof node that grants review to the caller
    When the caller sends a review task and waits for it to end
    And the caller sends the same task again
    Then the node returns the existing task
    And the agent ran 1 turn
    And the task is succeeded

  Scenario: An expired claim is refused before the agent starts
    Given a proof node that grants review to the caller
    When the caller sends a review task with an expired claim
    Then the node refuses the task because "the claim expired"
    And the agent ran 0 turns

  Scenario: Work without a local grant is refused before the agent starts
    Given a proof node with no grants
    When the caller sends a review task and waits for it to end
    Then the node refuses the task because "no local grant allows it"
    And the agent ran 0 turns

  Scenario: A task whose result was lost does not run again
    Given a proof node that lost the result of the caller's task
    When the caller sends a review task and waits for it to end
    Then the node refuses the task because "its earlier result is unknown"
    And the agent ran 0 turns

  Scenario: A full queue refuses new work before the agent starts
    Given a proof node whose queue is full with another caller's task
    When the caller sends a review task and waits for it to end
    Then the node refuses the task because "the queue is full"
    And the agent ran 0 turns
