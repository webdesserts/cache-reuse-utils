Feature: Full cache-reuse analysis
  CacheReuseAnalysis finds exact matches between a context and one borrowed
  cache, then returns the best legal placement found before one deadline.

  Scenario: An exact shared run becomes a plan
    Given a context and cache sharing a token run at least the minimum length
    When analysis runs
    Then the plan covers the run
    And the plan carries the caller's cache identity

  Scenario: No qualifying match is a deliberate empty plan
    Given a context and cache with no run at least the minimum length
    When analysis runs, even past the deadline when no work is possible
    Then analysis returns an empty plan with the cache identity

  Scenario: The longest matches survive the candidate cap
    Given more qualifying matches than the candidate capacity
    And one long match that is found last
    When analysis runs
    Then the plan uses the long match
    And discovery reports that the candidate limit was reached

  Scenario: Overlapping matches combine without a trim allowance
    Given two matches whose target ranges overlap
    When analysis runs with K of at least 2
    Then the later match is trimmed to start after the earlier one
    And the plan covers both

  Scenario: The protected prefix and fresh suffix stay fresh
    Given a match that crosses the protected prefix or the fresh suffix
    When analysis runs
    Then the plan reuses only the part between them

  Scenario: Padding meets alignment at the latest allowed boundary
    Given a match whose prepared start needs padding to be aligned
    And insertion is allowed in the gap before it
    When analysis runs with a padding policy
    Then the minimal padding is inserted at the latest allowed boundary
    And materializing the recipe reproduces the target with that padding

  Scenario: Padding is charged against coverage within the loss allowance
    Given a padded plan and a smaller plan without padding
    And the smaller plan is within the loss allowance of the best coverage
    When the pad penalty exceeds the coverage difference
    Then analysis returns the smaller plan without padding

  Scenario: A deadline returns the best plan found so far
    Given a search too large for the deadline
    When the deadline passes after a plan was found
    Then analysis returns that plan with a deadline-partial status
    But when no plan was found it returns a budget-exhausted error

  Scenario: Cancellation always wins
    Given a cancellation callback that returns true
    When it is observed at any stage, even after the deadline or with a plan
    Then analysis returns a cancelled error and no plan

  Scenario: Invalid requests are rejected before discovery
    Given K outside 1 to 10, a zero minimum, an invalid policy or an oversized input
    When analysis runs
    Then it returns a typed error at the preparation stage
