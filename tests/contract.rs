use scr_planner::{
    CandidateRange, ContractError, MAX_CANDIDATES, MAX_RANGES, MAX_SCRATCH_BYTES,
    PlanGeometryError, STACK_RESERVE_BYTES, scratch_bytes, validate_plan, validate_pool,
};

fn range(source_start: u32, destination_start: u32, token_count: u32) -> CandidateRange {
    CandidateRange {
        source_start,
        destination_start,
        token_count,
    }
}

#[test]
fn accepts_empty_pool_and_competing_alternatives_as_input() {
    let validated = validate_pool(&[], 1).expect("empty pool is valid");
    assert_eq!(validated.candidates(), &[]);
    assert_eq!(validated.scratch_bytes(), 0);
    assert_eq!(validated.reserved_bytes(), STACK_RESERVE_BYTES);

    // Destination overlap is normal competition; order is not significant.
    let pool = [range(20, 0, 10), range(0, 0, 8), range(40, 12, 3)];
    let validated = validate_pool(&pool, 1).expect("alternatives may conflict");
    assert_eq!(validated.candidates(), pool.as_slice());
}

#[test]
fn rejects_only_arithmetic_and_shape_faults_in_the_pool() {
    assert!(matches!(
        validate_pool(&[range(0, 0, 1)], 0),
        Err(ContractError::InvalidK {
            k: 0,
            max: MAX_RANGES
        })
    ));
    assert!(matches!(
        validate_pool(&[range(0, 0, 0)], 1),
        Err(ContractError::ZeroCount { index: 0 })
    ));
    assert!(matches!(
        validate_pool(&[range(u32::MAX, 0, 1)], 1),
        Err(ContractError::OffsetOverflow { index: 0 })
    ));
    let too_many = vec![range(0, 0, 1); MAX_CANDIDATES + 1];
    assert!(matches!(
        validate_pool(&too_many, 1),
        Err(ContractError::TooManyCandidates { .. })
    ));
}

#[test]
fn selected_plan_geometry_requires_order_disjointness_and_coalescing() {
    assert!(validate_plan(&[], 2).is_ok());
    assert!(validate_plan(&[range(0, 0, 4), range(8, 12, 3)], 2).is_ok());
    assert!(matches!(
        validate_plan(&[range(10, 0, 2), range(0, 3, 2)], 2),
        Err(PlanGeometryError::CrossingOrOverlap { index: 1 })
    ));
    assert!(matches!(
        validate_plan(&[range(0, 0, 4), range(4, 4, 2)], 2),
        Err(PlanGeometryError::AdjacentRanges { index: 1 })
    ));
    assert!(matches!(
        validate_plan(&[range(0, 0, 1), range(2, 2, 1)], 1),
        Err(PlanGeometryError::TooManyRanges { got: 2, max: 1 })
    ));
}

#[test]
fn scratch_models_peak_payload_without_capacity_rounding() {
    // Modeled payload: DP state 16 bytes each, 2 usize per candidate for order
    // and path tables, and three K-range plan buffers. Spare Vec capacity and
    // allocator overhead are deliberately not modeled.
    assert_eq!(
        scratch_bytes(1, 1),
        Some(2 * 16 + 2 * std::mem::size_of::<usize>() as u64 + 3 * 12)
    );
    assert_eq!(
        scratch_bytes(10, 2),
        Some(10 * (3 * 16 + 2 * std::mem::size_of::<usize>() as u64) + 3 * 2 * 12)
    );
    assert_eq!(scratch_bytes(0, 2), Some(0));
    assert_eq!(scratch_bytes(MAX_CANDIDATES + 1, 2), None);
    assert_eq!(scratch_bytes(1, MAX_RANGES + 1), None);

    let pool: Vec<_> = (0..MAX_CANDIDATES as u32)
        .map(|i| range(i * 3, i * 3, 2))
        .collect();
    let validated = validate_pool(&pool, MAX_RANGES).expect("max pool fits scratch");
    assert_eq!(validated.scratch_bytes(), 524_504);
    assert!(validated.scratch_bytes() <= MAX_SCRATCH_BYTES);
    assert_eq!(
        validated.reserved_bytes(),
        validated.scratch_bytes() + STACK_RESERVE_BYTES
    );
}
