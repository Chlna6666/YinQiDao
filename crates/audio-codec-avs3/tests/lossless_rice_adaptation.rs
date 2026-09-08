use yinqidao_codec_avs3::LosslessRiceState;

#[test]
fn rice_block_decay_uses_the_pre_block_sum_for_every_symbol() {
    // GB/T 33475.3's declared backward block-adaptive Golomb-Rice method updates
    // `sum` once per sub-block. Equation (7) subtracts the same pre-block
    // `sum / RICE_NUM_MUL` term for every symbol in that block. Recomputing the
    // quotient after each symbol would produce 227 here instead of the normative 224.
    let mut state = LosslessRiceState::new(3, 4).unwrap();
    assert_eq!(state.sum(), 256);

    state.update_mapped_residual_block(&[0, 0, 0, 0]).unwrap();

    assert_eq!(state.sum(), 224);
}

#[test]
fn rice_decoder_accepts_all_normative_sub_block_geometries() {
    for block_size in [2, 4, 8, 16, 32] {
        let state = LosslessRiceState::new(0, block_size).unwrap();
        assert_eq!(state.block_size(), block_size);
        assert_eq!(state.sum(), 32);
    }
}
