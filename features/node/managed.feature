Feature: Managed scopes
  In a managed scope, an administrator controls who is a member. The control
  service keeps the roster, and each node keeps a synchronized copy of it.

  Scenario: The test fixture makes a managed node
    When I make a test fixture for the "managed" scope
    Then the fixture policy is for the managed scope "managed"
    And the fixture authorization claims the scope "managed"

  @unix
  Rule: A member joins the roster with an invitation token

    Background:
      Given a running control service
      And a managed invitation token for the scope "managed"
      And a signed record for the member "managed-agent" in the scope "managed"

    Scenario: A redeemed member reaches the roster once
      When the member redeems the token
      And the node syncs the "managed" roster
      Then the sync reports 1 change
      When the node syncs the "managed" roster
      Then the sync reports 0 changes

    Scenario: A revoked member is marked as revoked on the next sync
      When the member redeems the token
      And the node syncs the "managed" roster
      And the control service revokes the member "managed-agent" in the scope "managed"
      And the node syncs the "managed" roster
      Then the node knows the first member of "managed" as revoked
      And the control service stops cleanly
