Feature: Cancellation
  A caller can cancel a task while the agent works on it. The task ends at
  once, and the node does not wait for the agent to finish its turn.

  Scenario Outline: Cancelling a running task ends it at once
    Given a proof node whose agent <behaviour>
    When the caller sends a review task
    And the agent turn starts
    And the caller cancels the task
    Then the task ends as cancelled with no result
    And the agent was asked to cancel
    And the task is cancelled

    Examples:
      | behaviour                                          |
      | waits until it is cancelled                        |
      | returns a late result after it is cancelled        |

  Scenario: A late result does not replace the cancelled state
    Given a proof node whose agent returns a late result after it is cancelled
    When the caller sends a review task
    And the agent turn starts
    And the caller cancels the task
    Then the task ends as cancelled with no result
    And the task events become "queued, leased, running, cancelled, result"
    And the task is cancelled
