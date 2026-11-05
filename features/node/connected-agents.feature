Feature: Connected agents
  The agent of a host does not have to be Codex. An agent such as the
  Hermes plugin connects to the agent socket of the node and answers the
  tasks. The caller sends the same command and gets the answer in the same
  way. The agent gets only what it needs to do the task.

  Background:
    Given a host node whose agent connects to the node
    And a caller with a signed authorization for the "direct" scope
    And the host node is running
    And the connected agent is ready
    And the caller imported the host's invitation

  Scenario: A connected agent answers a signed task
    When the caller sends the task "remote text"
    Then the caller gets the answer "connected agent result: remote text"
    And the host node stops cleanly

  Scenario: A connected agent gets no keys, grants or caller names
    When the caller sends the task "remote text"
    Then the connected agent got only the task id, the text and the instructions
    And the instructions tell the agent that the task text is untrusted
    And the host node stops cleanly
