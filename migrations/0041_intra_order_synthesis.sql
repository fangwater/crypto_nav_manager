CREATE OR REPLACE FUNCTION ensure_intra_order_synthesis(target_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    IF target_schema !~ '^[a-z][a-z0-9_]*$' THEN
        RAISE EXCEPTION 'invalid strategy schema: %', target_schema;
    END IF;

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.intra_orders (
            fkey BIGINT PRIMARY KEY,
            symbol TEXT NOT NULL,
            side TEXT NOT NULL CHECK (side IN (''buy'', ''sell'')),
            cts BIGINT NOT NULL,
            open_uts BIGINT NOT NULL,
            fts BIGINT,
            holding BIGINT NOT NULL,
            holding_close BIGINT,
            close_count BIGINT NOT NULL DEFAULT 0,
            price DOUBLE PRECISION NOT NULL,
            amount DOUBLE PRECISION NOT NULL,
            cprice DOUBLE PRECISION,
            camount DOUBLE PRECISION NOT NULL DEFAULT 0,
            range DOUBLE PRECISION NOT NULL,
            crange DOUBLE PRECISION NOT NULL DEFAULT -1,
            tlen DOUBLE PRECISION,
            pnlu DOUBLE PRECISION,
            open_fill_amount DOUBLE PRECISION NOT NULL,
            remaining_amount DOUBLE PRECISION NOT NULL,
            netted_amount DOUBLE PRECISION NOT NULL DEFAULT 0,
            close_notional DOUBLE PRECISION NOT NULL DEFAULT 0,
            matching_state TEXT NOT NULL
                CHECK (matching_state IN (''pending'', ''completed'', ''netted'', ''mixed'')),
            open_source_ts_us BIGINT NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            CHECK (cts > 0),
            CHECK (open_uts >= cts),
            CHECK (amount >= 0),
            CHECK (open_fill_amount > 0),
            CHECK (remaining_amount >= 0),
            CHECK (camount >= 0),
            CHECK (netted_amount >= 0)
        )',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.intra_orders
         (matching_state, symbol, side, open_uts, open_source_ts_us)',
        target_schema || '_intra_orders_pending_idx',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.intra_orders (open_source_ts_us)',
        target_schema || '_intra_orders_source_idx',
        target_schema
    );

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.intra_order_lifecycle (
            trading_venue TEXT NOT NULL,
            client_order_id BIGINT NOT NULL,
            first_source_ts_us BIGINT NOT NULL,
            last_source_ts_us BIGINT NOT NULL,
            create_ts_us BIGINT NOT NULL,
            update_ts_us BIGINT NOT NULL,
            symbol TEXT NOT NULL,
            side TEXT NOT NULL,
            price DOUBLE PRECISION NOT NULL,
            price_offset DOUBLE PRECISION NOT NULL,
            amount_init DOUBLE PRECISION NOT NULL,
            filled_amount DOUBLE PRECISION NOT NULL,
            fill_notional DOUBLE PRECISION NOT NULL,
            status TEXT NOT NULL,
            from_key TEXT NOT NULL,
            event_count BIGINT NOT NULL,
            terminal BOOLEAN NOT NULL,
            PRIMARY KEY (trading_venue, client_order_id)
        )',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.intra_order_lifecycle
         (terminal, last_source_ts_us)',
        target_schema || '_intra_lifecycle_ready_idx',
        target_schema
    );

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.intra_hedges (
            client_order_id BIGINT PRIMARY KEY,
            main_fkey BIGINT,
            symbol TEXT NOT NULL,
            side TEXT NOT NULL CHECK (side IN (''buy'', ''sell'')),
            create_ts_us BIGINT NOT NULL,
            update_ts_us BIGINT NOT NULL,
            source_ts_us BIGINT NOT NULL,
            amount DOUBLE PRECISION NOT NULL,
            cprice DOUBLE PRECISION,
            event_count BIGINT NOT NULL,
            allocated_amount DOUBLE PRECISION NOT NULL,
            unallocated_amount DOUBLE PRECISION NOT NULL,
            anchor_matched BOOLEAN NOT NULL,
            processed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
        )',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.intra_hedges (source_ts_us)',
        target_schema || '_intra_hedges_source_idx',
        target_schema
    );

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.intra_match_watermark (
            singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
            source_read_through_us BIGINT NOT NULL,
            events_released_through_us BIGINT NOT NULL,
            margin_finalized_through_us BIGINT NOT NULL,
            verified_through_ms BIGINT NOT NULL,
            reorder_window_us BIGINT NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
        )',
        target_schema
    );
END;
$$;

DO $$
DECLARE
    target_schema TEXT;
    start_ms BIGINT;
BEGIN
    FOR target_schema, start_ms IN
        SELECT s.db_schema, c.aligned_from_ms
        FROM strategy_envs s
        JOIN rocksdb_alignment_checkpoints c ON c.strategy_slug = s.slug
        WHERE s.slug IN (
            'binance-intra-arb01',
            'bybit-intra-arb01',
            'bybit-intra-arb02'
        )
    LOOP
        PERFORM ensure_intra_order_synthesis(target_schema);
        EXECUTE format(
            'INSERT INTO %I.intra_match_watermark (
                source_read_through_us,
                events_released_through_us,
                margin_finalized_through_us,
                verified_through_ms,
                reorder_window_us
             ) VALUES ($1, $1 - 1, $1 - 1, $2, 600000000)
             ON CONFLICT (singleton) DO NOTHING',
            target_schema
        ) USING start_ms * 1000, start_ms;
    END LOOP;
END;
$$;

DROP FUNCTION ensure_intra_order_synthesis(TEXT);

COMMENT ON COLUMN binance_intra_arb01.intra_match_watermark.source_read_through_us IS
    'Half-open RocksDB key cursor. The next center read starts exactly here.';
COMMENT ON COLUMN binance_intra_arb01.intra_match_watermark.margin_finalized_through_us IS
    'Largest continuous source-time prefix not blocked by a pending real Margin fill.';
