# Performance Baselines

Every number here is a **CI gate**. A milestone is not complete until its section passes on both the desktop profile and the memory-constrained profile.

Reference hardware: 8 physical cores, 32 GB RAM, mid-range discrete GPU. Profiles are *desktop* (12 GB) and *min-spec* (8 GB); see `memory-budget.md`. Numbers are 95th-percentile over 1,000 runs unless noted. When hardware changes, re-baseline in one commit and note the change here — never silently loosen a gate.

## Baseline changes

Re-baselining is a deliberate act, recorded here with its reason. A gate that moves without
a note is a gate that stops meaning anything.

| Date | Gate | From | To | Why |
|---|---|---|---|---|
| 2026-08-17 | `alloc_per_tick_steady_state` | 0 combined | split in two | Measured 13 allocations per tick with zero systems, plus one per system, all of it inside bevy_ecs's multi-threaded executor. Engine code allocates exactly zero, proven by the same schedule running allocation-free single-threaded. A literal combined zero would mean forking the ECS ADR-0001 chose not to fork. Split so the strict zero still guards our code and the executor gets a visible ceiling — see `ADR-0014`. A single loosened threshold would have hidden the failure that matters: five allocations from a sim system sitting unnoticed inside a budget of twenty. |
| 2026-08-17 | `ecs_spawn_batch_100k_speedup` | 20x | 1.75x | Measured 1.9x on `bevy_ecs` 0.19, where a single spawn costs ~24 ns and a batched one ~12 ns. The ratio held at 1.9x for two components into an empty world (1.24 ms vs 2.36 ms) and for four components into a world already holding 200k entities (1.70 ms vs 3.22 ms), so it is not a scenario artefact. The original 20x described an ECS where per-spawn archetype moves dominate; this one caches the archetype lookup. The gate's intent is unchanged — bulk spawn is the path agents and chunk activation use, and 1.75x still fails loudly if `spawn_batch` ever loses its advantage. |

## m0

| Benchmark | Target | Spec |
|---|---|---|
| `ecs_iterate_1m_2comp` | < 3 ms, 1 thread | S02 |
| `ecs_tick_1m_3systems` | < 33 ms, 8 threads | S02 |
| `ecs_spawn_batch_100k_speedup` | ≥ 1.75x vs loop (see baseline changes) | S02 |
| `field_stencil_16m_cells` | < 12 ms, 8 threads | S06 |
| `field_halo_exchange_16_chunks` | < 1 ms | S06 |
| `alloc_per_tick_sim_code` | 0, single-threaded | S02, S06, `ADR-0014` |
| `alloc_per_tick_executor` | ≤ 16 + systems | `ADR-0014` |
| `module_resolution_order_independence` | identical schedule hash, 10 shuffles | S20 |
| `disabled_module_zero_cost` | 0 tick time, 0 bytes | S20 |
| `per_module_smoke_profiles` | all pass | S20 |
| `determinism_threads_1_4_16` | exact match, 10k ticks (`cargo test -p cx-diag`) | S14 |
| `determinism_subprocess` | exact match, 10k ticks (`cargo test -p cx-diag`) | S14 |
| `memory_16_chunks_1m_entities` | < 8 GiB peak RSS, Linux | `memory-budget.md` |
| `graph_export_byte_identical` | exact, 10 shuffled orders | S21 |
| `graph_export_minimal_profile` | < 500 ms, no tick run | S21 |

## m1

| Benchmark | Target | Spec |
|---|---|---|
| `render_100k_instances_fps` | ≥ 60 fps, < 20 draw calls | S12 |
| `frame_time_p99_30hz_sim_144hz_render` | < 8 ms | S03 |
| `extract_100k_instances` | < 2 ms | S12 |
| `debug_draw_10k_lines` | < 1 ms | S12 |
| `headless_vs_windowed_hash` | exact, 10k ticks | S03 |
| `graph_layout_stable_across_runs` | pixel-identical | S21 |
| `graph_diff_no_false_positives` | 0 on unchanged commit | S21 |

## m2

| Benchmark | Target | Spec |
|---|---|---|
| `block_generate_16384sq_full_pipeline` | < 20 s, 8 background threads | S07 |
| `no_erosion_profile_generates_valid_world` | passes | S20, S07 |
| `worldgen_order_independence_4x4_blocks` | exact hash match | S07 |
| `chunk_extract_from_cached_block` | < 5 ms | S07 |
| `terrain_mesh_bake_one_chunk` | < 200 ms offline | S12 |
| `traversal_200ms_frontier_keeps_up` | never outrun, no frame > 20 ms | S07 |
| `block_cache_delete_and_replay` | identical world state | S07 |
| `dormant_10k_chunks_memory` | within budget | S07 |

## m3

| Benchmark | Target | Spec |
|---|---|---|
| `content_load_10k_prototypes_cached` | < 200 ms | S04 |
| `content_cache_vs_parse_speedup` | ≥ 10x | S04 |
| `material_count_50k_objects` | < 10 | S04, S12 |

## m4

| Benchmark | Target | Spec |
|---|---|---|
| `discharge_routing_lag_100km` | plausible, verified | S08 |
| `flood_tier_lookup_per_chunk` | < 1 ms | S08 |
| `water_surface_seam_continuity` | continuous | S08 |
| `finite_body_fill_overflow_drain` | within tolerance | S08 |
| `ecology_soil_stencil_16m_cells` | < 33 ms | S08 |
| `no_nan_any_field_1m_ticks` | zero | S08 |

## m4b

| Benchmark | Target | Spec |
|---|---|---|
| `edit_tile_patch_mesh_latency` | < 1 ms, same frame | S19 |
| `edit_chunk_rebake_swapin` | < 5 frames | S19 |
| `edit_collider_height_update` | < 0.5 ms in place | S19 |
| `edit_navgrid_tile_update` | < 0.5 ms | S19 |
| `edit_1000_in_one_tick` | no frame > 20 ms | S19 |
| `impoundment_fills_to_spill` | exact, no further | S19 |
| `drainage_repair_bounded_one_block` | verified | S19 |
| `save_100k_edits` | < 5 MB | S19 |
| `edit_undo_hash_match` | exact | S19 |
| `levelto_platform_flatness` | within 1 cell | S19 |
| `dig_depth_clamp` | clamps, reports to UI | S19 |

## m5

| Benchmark | Target | Spec |
|---|---|---|
| `tick_10k_chunks_vs_16_chunks` | within 20% | S09 |
| `aggregate_roundtrip_conservation` | exact totals | S09 |
| `fast_forward_1m_ticks_one_chunk` | < 500 ms | S09 |
| `time_accel_10000x_pacing` | sustains real-time | S03, S09 |
| `active_vs_fastforward_agreement` | within 5% | S08, S09 |

## m6

| Benchmark | Target | Spec |
|---|---|---|
| `spatial_rebuild_1m_agents` | < 8 ms, 8 threads | S05 |
| `spatial_query_100k_radius10` | < 5 ms | S05 |
| `agents_100k_full_tier` | < 15 ms, 8 threads | S10 |
| `agents_1m_mixed_tier` | < 33 ms | S10 |
| `flow_field_rebuild_one_chunk` | < 3 ms | S10 |

## m7

| Benchmark | Target | Spec |
|---|---|---|
| `save_10k_unmodified_chunks` | < 100 KB | S13 |
| `save_excludes_block_cache` | verified | S13 |
| `autosave_500mb_max_frame` | < 20 ms | S13 |
| `replay_100k_ticks_hash` | exact | S13 |
| `state_hash_1m_entities_16m_cells` | < 2 ms | S14 |
| `divergence_detect_injected_bug` | < 30 s | S14 |
| `inspector_frame_time_1m_entities` | < 16 ms | S14 |

## m8

| Benchmark | Target | Spec |
|---|---|---|
| `physics_5k_bodies_step` | < 8 ms | S11 |
| `heightfield_build_one_chunk_cached` | < 5 ms, once | S11 |
| `physics_disabled_overhead` | 0 measurable | S11 |

## m9

| Benchmark | Target | Spec |
|---|---|---|
| `animated_50k_instances_vat` | ≥ 60 fps | S15 |
| `headless_vs_windowed_hash_full_game` | exact | S15 |
| `presentation_budget_at_1000x` | within voice/particle caps | S15 |
| `script_budget_overrun_tick_impact` | 0 | S17 |

## m10

| Benchmark | Target | Spec |
|---|---|---|
| `render_1m_instances_mixed_lod` | ≥ 60 fps | S12 |
| `cold_asset_load_from_bundle` | < 3 s | S18 |
| `runtime_shader_compiles_release` | 0 | S18 |
