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

  Scenario: Stateless source tokens are actual content
    Given a cache view containing historical artificial or whitespace-like token values
    When placement runs
    Then those tokens are eligible source content
    And no token value is classified as padding
    And only insertions made by this analysis count toward padding

  Scenario: Canonical insertion in a fresh gap
    Given a placement policy allows insertions at several boundaries in one fresh gap
    And the next reused span requires a current-call pad count
    When a variant is placed
    Then the pads are inserted at the latest allowed boundary before that span
    And the prepared ranges and insertion record reflect that canonical choice

  Scenario: Minimal current-call pads dominate alignment multiples
    Given one fixed pad token, no per-boundary cap and no position-dependent constraints
    When the smallest pad count satisfies target alignment and minimum prepared start
    Then adding a full alignment multiple cannot improve future residues or K
    And the minimum count is used

  Scenario: Relative allowance retains a lower-padding alternative
    Given a completed plan of 980 useful tokens requiring two pads
    And an 850-token plan below the 882 ten-percent floor
    And a 900-token zero-pad plan within the floor
    Then the selected plan uses 900 useful tokens and zero pads

  Scenario: Trim rectangle is explicit and bounded
    Given a maximal match and caller-declared left/right trim bounds
    When placement variants are generated
    Then every non-empty trim inside that rectangle is considered
    And completeness is claimed only over that declared domain

  Scenario: Complete no-feasible placement is not a discovered no-match
    Given maximal matches exist but every declared placement variant is infeasible
    When search completes without a deadline or resource stop
    Then the empty plan reports NoFeasibleReuse
    And a complete no-match remains a distinct deliberate empty result

  Scenario: Variant or state limit returns a completed plan honestly
    Given a valid completed plan exists when a placement bound is reached
    Then the best completed plan is returned
    And its placement status reports the reached limit
    And no empty plan is fabricated

  Scenario: One hundred percent allowance empty edge
    Given no positive completed plan exists when the deadline or a resource limit stops search
    Then analysis returns BudgetExhausted or the limiter error, not an empty plan
    And after a positive completed plan establishes B, an empty zero-pad plan may compete at 100%

  Scenario: Capacity is cumulative across multiple fresh gaps
    Given a plan needs current-call insertions in more than one gap
    When placement checks capacity
    Then original target length plus all accumulated padding is within the declared maximum
    And a per-transition check alone is insufficient

  Scenario: Variant derivation is interruptible
    Given a large declared trim rectangle whose variants may all be rejected
    When cancellation or the absolute deadline is observed during nested trim iteration
    Then derivation stops immediately with the typed stop
    And search does not start on an unverified variant bag
    And there is no work-counter or modulo polling rule

  Scenario: Completed plans survive state admission limits
    Given a fully computed plan has been published to its padding bucket
    When optional extension-state or modeled payload admission reaches its cap
    Then the completed plan remains available with an explicit state-limit status
    And a cap before any completed plan returns an informative limiter error
