# test(giveaway): add two edge-case tests for claim lifecycle

## What changed

### `test.rs`
Added two tests to cover gaps in the existing claim lifecycle suite:

**`test_pick_winner_emits_winner_selected_event`**
Verifies that `pick_winner` emits a `GiveawayWinnerSelected` event with the correct topics (`giveaway`, `winner`, winner address) and that the `prize_amount` in the data vec equals the full gross share before any fee deduction.

**`test_partial_claim_creator_receives_exact_unclaimed_gross_shares`**
3-winner giveaway where only winner-0 claims before the deadline. After the window expires, asserts the creator recovers exactly the two unclaimed gross shares with no fee deduction on recovery, and the contract retains only the fee from the single successful claim.

## Test results
- 143 → 145 passing, 0 failing
