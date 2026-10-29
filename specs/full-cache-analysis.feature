Feature: Full cache-reuse analysis
  CacheReuseAnalysis discovers exact token matches between a target context and
  one borrowed cache slice, then selects the highest-value legal placement
  found before a single cooperative deadline.

  Scenario: An exact shared run produces a reuse plan
    Given a context and cache that share a token run of at least the minimum length
    When analysis runs
    Then the returned plan covers that shared run
    And the plan preserves the caller's cache identity

  Scenario: An input with no qualifying match returns a complete empty plan
    Given a context and cache with no shared run of at least the minimum length
    When analysis runs, even if the deadline has already passed when no match is possible
    Then analysis returns an empty plan carrying the caller's cache identity
    And the placement status is Complete

  Scenario: The longest matches survive when candidates exceed capacity
    Given more qualifying matches than the 4,096-candidate capacity
    And one long match that appears after thousands of shorter matches
    When analysis runs
    Then the selected plan uses the long match
    And discovery reports CandidateLimitReached

  Scenario: Overlapping matches chain together without a trim allowance
    Given two matches whose target or source spans overlap
    When analysis runs with K of at least 2
    Then the second match is left-trimmed to start after the first
    And the resulting plan reuses tokens from both matches

  Scenario: The protected prefix and fresh suffix remain uncached
    Given a shared match that extends into the protected prefix or the fresh suffix
    When analysis runs
    Then the plan reuses only the portion inside the reusable middle window

  Scenario: Padding aligns a match at the latest allowed boundary
    Given a match whose prepared target start requires padding to meet alignment
    And padding insertion is permitted at a boundary in the gap before the match
    When analysis runs with a padding policy
    Then the smallest required padding is inserted at the latest allowed boundary
    And materializing the recipe reproduces the target tokens with that padding inserted

  Scenario: Padding penalty trades off against coverage within the loss allowance
    Given a padded plan and a slightly shorter unpadded plan
    And the unpadded plan's coverage is within the loss allowance of the best coverage
    When the pad penalty for the inserted padding exceeds the coverage difference
    Then analysis selects the unpadded plan

  Scenario: Up to 32 ranges reuse a prompt made of many unchanged blocks
    Given a prompt in which 20 unchanged blocks each moved to a different offset
    When analysis runs with K of 32
    Then the plan reuses every block as its own range

  Scenario: Each extra range must earn its place within the loss allowance
    Given a plan that reuses a few long blocks
    And a plan that also reuses several short stray matches between them
    And both plans are within the loss allowance of the best coverage
    When the extra ranges cover fewer tokens than the range penalty charges for them
    Then analysis selects the plan with fewer ranges
    But a single short match is never rejected for being a range, because the first range is free

  Scenario: An unaligned match can be left-trimmed instead of padded
    Given a match whose prepared target start is not aligned
    When analysis runs
    Then the planner also considers left-trimming the match to the next aligned prepared start
    And the left-trimmed placement requires no padding and no trim allowance
    And the planner selects the left-trimmed placement when it yields higher net coverage after the pad penalty
    And the planner selects the left-trimmed placement when padding is not allowed before the match

  Scenario: Matches that neither padding nor left-trimming can place report no feasible reuse
    Given qualifying matches that cannot satisfy alignment or minimum-length rules via padding or left-trimming
    When analysis runs
    Then analysis returns an empty plan with status NoFeasibleReuse

  Scenario: Reaching the deadline after finding a plan returns the best plan found so far
    Given a search space too large to finish before the deadline
    When the deadline expires after at least one valid plan has been found
    Then analysis returns the best plan found so far with status DeadlinePartial
    But if the deadline expires before any valid plan is found, analysis returns a BudgetExhausted error

  Scenario: Cancellation takes precedence over deadlines and partial plans
    Given a cancellation callback that returns true
    When cancellation is observed at any stage, even after a plan was found or the deadline passed
    Then analysis returns a Cancelled error and discards any partial plan

  Scenario: Slice materialization populates prepared tokens and physical pad positions without allocating
    Given a selected reuse plan that inserts padding before one or more reused ranges
    When the caller materializes the recipe into pre-allocated prepared-token and pad-position slices
    Then each Insertion's prepared_start equals its original target boundary plus all earlier insertions
    And every reused range matches the corresponding source and target tokens
    And projecting the prepared tokens with those pad positions recovers the exact original target tokens

  Scenario: Common logical prefix skips interleaved pads in a cached physical sequence
    Given a cached physical token sequence and its strictly increasing pad positions
    When a new logical prompt is compared with common_logical_prefix
    Then the helper reports the number of matching leading logical tokens and the pads carried inside that prefix
    And any pad positions after the last matching logical token are excluded from the kept physical prefix

  Scenario: Invalid requests are rejected during preparation
    Given K outside 1..=32, a zero minimum match length, an invalid placement policy, or an oversized input
    When analysis runs
    Then analysis fails at the Preparation stage with a typed validation or admission error
