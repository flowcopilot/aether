Feature: Owner links
  A person can show that they own a service agent. The person's agent
  signs a claim on the service agent, and the service agent signs to
  confirm it. Then the lookup and the directory show the person's full
  name as the owner, but never the person's endpoint. A service agent has
  at most one owner, and a block, a new name, a deleted account or the
  expiry ends the link.

  Background:
    Given ana is a person
    And law is a listed service agent

  Rule: A person claims and the service agent confirms

    Scenario: A person claims a service agent
      When ana claims law
      Then the hub answers 201
      And the answer is an open claim for the person, without the endpoint of law
      And a lookup of law shows no owner
      And law sees the claim from ana, with the endpoint of ana

    Scenario: The service agent confirms the claim
      Given ana claimed law
      When law confirms the claim
      Then the hub answers 200
      And the answer is a confirmed link
      And ana sees the confirmed link, with the endpoint of law
      And a lookup of law shows ana as the owner, without the endpoint of ana

  Rule: The signatures and the expiry must be valid

    Scenario Outline: A bad claim is refused
      When ana claims law <problem>
      Then the hub answers 400
      And ana has no links

      Examples:
        | problem                  |
        | signed by another key    |
        | for another name         |
        | that expires in 366 days |
        | that expired a day ago   |

    Scenario: A claim can last up to 365 days
      When ana claims law for 365 days
      Then the hub answers 201

    Scenario Outline: A bad confirmation is refused
      Given ana claimed law for 365 days
      When law confirms the claim <problem>
      Then the hub answers 400
      And law sees the claim still open

      Examples:
        | problem                    |
        | signed by another key      |
        | with another expiry        |
        | for another personal agent |

  Rule: Only a person claims, and only the claimed service agent confirms

    Scenario: A service agent cannot claim another service agent
      Given lex is a service agent
      When lex claims law
      Then the hub answers 403

    Scenario Outline: A claim on something that is not a service agent is not found
      When ana claims <target>
      Then the hub answers 404

      Examples:
        | target                 |
        | a person               |
        | the name "nobody#0001" |

    Scenario: A person cannot confirm her own claim
      Given ana claimed law
      When ana confirms her own claim
      Then the hub answers 403

    Scenario: Another service agent does not find the claim
      Given ana claimed law
      And lex is a service agent
      When lex confirms the claim
      Then the hub answers 404

    Scenario: A confirmed link cannot be confirmed again
      Given ana claimed law
      And law confirmed the claim
      When law confirms the claim
      Then the hub answers 403

  Rule: A service agent has one owner

    Scenario: A second owner waits until the first link ends
      Given bob is a person
      And ana claimed law
      And bob claimed law
      And law confirmed the claim of ana
      When law confirms the claim of bob
      Then the hub answers 409 with the error "limit"
      When law deletes the link of ana
      Then the hub answers 204
      When law confirms the claim of bob
      Then the hub answers 200
      And a lookup of law shows bob as the owner

  Rule: A block hides and ends the link

    Scenario: A claim from a blocked person is hidden from the service agent
      Given law blocks ana
      When ana claims law
      Then the hub answers 201
      And ana sees her claim
      And law has no links
      When law confirms the claim
      Then the hub answers 404
      When law deletes the link of ana
      Then the hub answers 404

    Scenario: Blocking the owner ends the link
      Given ana claimed law
      And law confirmed the claim
      When law blocks ana
      Then ana has no links
      And a lookup of law shows no owner

  Rule: A link ends with a deletion, a new name or a deleted account

    Scenario: The owner deletes the link
      Given ana claimed law
      And law confirmed the claim
      When ana deletes her link
      Then the hub answers 204
      And law has no links

    Scenario: A new name of the service agent ends the claim
      Given ana claimed law
      When law takes a new name
      Then the hub answers 200
      And ana has no links

    Scenario: A deleted account of the person ends the claim
      Given ana claimed law
      When ana deletes her account
      Then the hub answers 204
      And law has no links

  Rule: A link expires, and an open claim lasts 7 days

    Scenario: An expired link no longer shows
      Given ana claimed law
      And law confirmed the claim
      When the link expires
      Then ana has no links
      And a lookup of law shows no owner

    Scenario: A claim older than 7 days no longer shows
      Given bob is a person
      And bob claimed law
      When the claim of bob is 8 days old
      Then bob has no links

    Scenario: The sweep removes expired links and old claims
      Given bob is a person
      And ana claimed law
      And law confirmed the claim
      And the link expires
      And bob claimed law
      And the claim of bob is 8 days old
      When the hub sweeps expired links
      Then the hub keeps no links for law

  Rule: A person has at most 5 open claims and 20 links

    Scenario: The sixth open claim is refused
      Given ana has open claims on 5 service agents
      When ana claims one more service agent
      Then the hub answers 409 with the error "limit"

    Scenario: A person with 20 links cannot claim more
      Given ana has 20 links
      When ana claims law
      Then the hub answers 409
