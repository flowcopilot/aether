Feature: Tasks that run at once
  The owner of a node says how many tasks the node runs at the same time,
  for all callers together. A new node, and a node from before the limit
  existed, runs one task at a time and the others wait their turn. The
  waiting limits of the queue are a separate setting.

  Background:
    Given a host node whose agent takes a second to answer
    And the host node keeps 4 waiting tasks
    And 2 callers with a signed authorization for the "direct" scope

  Scenario: By default the node runs one task at a time
    Given the host node is running
    When 2 callers send a task at the same time
    Then one task runs while the other waits
    And the host node stops cleanly

  Scenario: A node whose owner allows two runs two tasks at once
    Given the host node runs 2 tasks at once
    And the host node is running
    When 2 callers send a task at the same time
    Then both tasks run at the same time
    And the host node stops cleanly
