Feature: Request size limit
  The hub reads at most 16 KiB of a request body. A larger body costs the
  hub memory for no use, so the hub refuses it before it reads it all.

  Scenario: A body over 16 KiB is refused
    When a node sends a body of 16 KiB and a few bytes more
    Then the hub answers 413 with the error "too_large"

  Scenario: A body of exactly 16 KiB is read
    When a node sends a body of exactly 16 KiB that is not a valid claim
    Then the hub answers 400
