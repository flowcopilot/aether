Feature: Directory changes
  The owner can tell a running node to renew its directory record now.
  The node takes only the directory that it already trusts, so a change
  cannot move the node to a directory whose certificate it did not pin.

  Background:
    Given a directory service is running
    And a host node whose agent is Codex
    And a caller with a signed authorization for the "directory" scope
    And the host node lists itself in the directory as "server" for 3600 seconds at a time
    And the host node is running
    And a reader gets 1 record from the directory

  Scenario: The owner renews the record with the directory that the node uses
    When the owner tells the node to use the directory that it uses now
    Then the host node applies the change
    And a reader gets 1 record from the directory

  Scenario: The owner cannot move the node to another directory
    When the owner tells the node to use the directory "https://wrong.test"
    Then the host node refuses the change as invalid
