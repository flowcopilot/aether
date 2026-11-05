Feature: Task queue
  A node keeps each task in a durable queue with a limit. The queue keeps
  one task for one request, refuses work when it is full, and never runs a
  task twice when it cannot know if the first run finished.

  Background:
    Given a task store whose queue holds one task
    And the caller has queued task "one"

  Scenario: The same request returns the task that is already queued
    When the caller sends task "one" again
    Then the store returns the existing task "one"

  Scenario: A different request with the same key is refused
    When the caller sends another request with the key of task "one"
    Then the store refuses it because "the request does not match the key"

  Scenario: A full queue refuses another task
    When the caller queues task "two"
    Then the store refuses it because "the queue is full"

  Scenario: A cancelled task makes room in the queue
    When the caller cancels task "one"
    And the caller queues task "restart"
    Then task "one" is cancelled
    And the store accepts task "restart"

  Scenario: A task whose lease expires before it runs goes back to the queue
    Given the caller cancels task "one"
    And the caller has queued task "restart"
    When a worker leases task "restart" in turn "turn-one"
    And the lease expires
    Then task "restart" is queued
    And a worker can lease task "restart" again in turn "turn-two"

  Scenario: A task that was running when its lease expired is not run again
    Given the caller cancels task "one"
    And the caller has queued task "restart"
    And a worker leases task "restart" in turn "turn-one"
    And the lease expires
    And a worker leases task "restart" in turn "turn-two"
    And task "restart" starts running in turn "turn-two"
    When the lease expires
    Then the result of task "restart" is unknown
    And no worker can lease a task
    And the events of task "restart" after sequence 4 are "running, unknown_result"
