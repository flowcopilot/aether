Feature: Database upgrades
  An operator upgrades a running hub to a new version. The hub upgrades its
  database at startup, and it keeps the accounts and invites that people
  already have.

  Scenario: The names upgrade keeps each account and its invites, and gives each account a number
    Given a hub database from before accounts had numbers
    And these accounts:
      | key | name  | version |
      | e1  | alice | 3       |
      | e2  | bob   | 1       |
    And a pending invite from alice to bob
    When the hub upgrades its database to the names version
    Then each account keeps its key and name and is a personal agent
    And each account has a number from 1 to 9999
    And the version of each account moved up by one
    And no two accounts have the same tag
    And the invite is still pending
