Feature: Directory discovery
  A host node publishes a signed record of its address in a directory. A
  caller finds the host by its agent id, so the caller does not need to
  know the address. The node renews the record before it expires, and the
  caller keeps the record it found in case the directory stops.

  Background:
    Given a directory service is running
    And a host node whose agent is Codex
    And a caller with a signed authorization for the "directory" scope
    And the host node lists itself in the directory as 'server "one"' for 4 seconds at a time
    And the host node is running

  Scenario: The node publishes its record under an agent id with a quote and a space
    Then the host node reports its published record for 'server "one"'
    And the directory gives the endpoint and the address of the host node

  Scenario: The node renews its record before it expires
    Given the directory gives the endpoint and the address of the host node
    When the record is 3 seconds old
    Then the directory gives a record that expires later

  Scenario: A caller finds the host through the directory
    Given the caller finds the host through the directory
    When the caller sends the task "remote text"
    Then the caller gets the answer "fake Codex result"
    And the host node stops cleanly

  Scenario: A caller still reaches the host when the directory stops
    Given the caller finds the host through the directory
    And the caller sent the task "remote text"
    And the caller got the answer "fake Codex result"
    When the directory service stops
    And the caller sends the task "remote text"
    Then the caller gets the answer "fake Codex result"
    And the host node stops cleanly
