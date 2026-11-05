Feature: Task events
  A node tells subscribers about each change to a task. A slow subscriber
  must not hold back the work, and it can read the events it missed from
  the node's record.

  Background:
    Given a proof node that grants review to the caller
    And an event subscriber that reads no events
    When the caller sends 64 review tasks one after another

  Scenario: A slow subscriber does not hold back the work
    Then the subscriber has fallen behind
    And the last task succeeded

  Scenario: A subscriber that fell behind reads the stored events
    Then the events of the last task are "queued, leased, running, result, succeeded"
    And the events of the last task after its second event are "running, result, succeeded"
