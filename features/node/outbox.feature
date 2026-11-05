Feature: Outbox
  The node of the caller keeps the tasks of its agent in an outbox. When
  the host cannot take a task yet, the outbox sends it again later and
  keeps the answer, so the caller does not send the task again by hand.

  Background:
    Given a host node whose agent takes a second to answer
    And a caller with a signed authorization for the "direct" scope
    And the host node is running
    And the caller saved the host's invite in its contacts
    And the caller put 3 tasks in its outbox

  Scenario: A task waits when the queue of the host is full
    When the outbox sends the 3 tasks at once
    Then the third task waits because "the queue of the agent is full"
    And the host node stops cleanly

  Scenario: The outbox sends again and keeps every answer
    Given the outbox sent the 3 tasks at once
    When the outbox runs until it has no more work
    Then every task in the outbox has the answer "fake Codex result"
    And the host node ran 3 tasks
    And the host node stops cleanly
