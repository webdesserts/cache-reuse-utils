Feature: Full cache-reuse analysis
  CacheReuseAnalysis discovers exact logical matches from one borrowed cache
  source, then selects a legal plan under the same absolute deadline.

  Scenario: Exact qualifying match becomes a plan
    Given a context and cache sharing an exact logical token run at least the minimum
    When analysis runs before the deadline
    Then the returned plan covers the match
    And the plan carries the caller's cache identity

  Scenario: Repeated and adjacent matches remain competing candidates
    Given a context and cache with repeated overlapping occurrences and adjacent runs
    When discovery completes before the deadline
    Then all qualifying maximal matches are available to the selector
    And final ranges are legal and coalesced according to K

  Scenario: Minimum match length filters candidates
    Given a minimum match length larger than some exact runs
    When discovery completes
    Then only matches at least that long are considered

  Scenario: Complete no-match analysis is deliberate no-reuse
    Given context and cache tokens with no qualifying match
    When discovery completes before the deadline
    Then analysis returns a valid empty plan
    And discovery is marked complete

  Scenario: No-work no-match analysis is complete immediately
    Given empty context or cache, or a minimum longer than an input
    When analysis starts, even past the deadline
    Then analysis returns a valid empty plan
    And no discovery or search duration is fabricated

  Scenario: Empty plan keeps cache identity
    Given a complete no-match analysis
    Then the empty plan returns the caller's cache identity

  Scenario: Deadline during discovery does not fabricate a plan
    Given discovery has not completed and no valid plan exists
    When the absolute deadline is observed
    Then the result is BudgetExhausted at the Discovery stage
    And no search starts after the deadline

  Scenario: Candidate capacity limit retains verified candidates
    Given more qualifying maximal matches than the candidate capacity
    When discovery stops at that capacity before the deadline
    Then the returned plan is selected from verified candidates
    And the plan reports that candidate capacity was reached

  Scenario: Cancellation wins at any stage
    Given cancellation becomes true during preparation, discovery or search
    Then the result is Cancelled with its stage and timing
    And no plan is returned even if candidates or an incumbent exist

  Scenario: Invalid view or K is rejected
    Given zero minimum, invalid K, or an input beyond the supported envelope
    Then analysis returns an informative non-plan error without panicking

  Scenario: Background timer expiry stops discovery
    Given the background deadline owner marks the analysis deadline
    When discovery observes that flag at a natural loop point
    Then analysis stops at Discovery without starting search
    And the timer registration is removed before returning

  Scenario: Expiry after discovery with verified candidates does not start search
    Given discovery produced verified candidates
    And the deadline flag is already set before search entry
    When analysis reaches the search boundary
    Then it returns BudgetExhausted at Search with no search duration

  Scenario: Cancellation wins over simultaneous timer expiry
    Given the deadline flag is set and cancellation is true at the same check
    Then analysis returns Cancelled, not BudgetExhausted
    And no plan is returned

  Scenario: Timer registration failure is an admission error
    Given the background timer owner cannot be started
    When analysis has real work to perform
    Then it returns AdmissionDenied at Preparation with TimerUnavailable timing
    And it does not fall back to loop clock comparisons
