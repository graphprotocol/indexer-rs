class TapSenderAccount:
    def __init__(self, sender_id: str, db_connection, fee_col: str = "grt_total"):
        self._sender_id = sender_id
        self._db = db_connection
        self._fee_col = fee_col
        self._watermark = 0  # The index/block to gate updates
        self._lock = asyncio.Lock()
        self._is_initialized = True

    async def start(self):
        """
        Loads the DB state into memory and ensures the watermark matches.
        Fixes the 'silent death' by ensuring the 'watermark' gates the
        update logic so we don't re-process stale receipts or freeze on
        the initial high value.
        """
        # 1. Sync initial DB state into memory
        initial = await self._db.fetch_one(
            "SELECT grt_total, receipt_block FROM tap_horizon_receipts WHERE sender_id = %s LIMIT 1",
            (self._sender_id,)
        )
        if initial:
            self._current_total = float(initial[self._fee_col]) if initial[self._fee_col] else 0.0
            self._watermark = int(initial["receipt_block"]) if "receipt_block" in initial else 0
        else:
            self._current_total = 0.0
            self._watermark = 0

        # 2. Start the infinite stream (or event loop)
        # We use an async iterator for the source stream
        async for receipt in self._db.stream_source("tap_horizon_receipts", self._sender_id):
            # 3. The 'Silent' Fix: Gate the update on the watermark
            # This ensures we don't skip updates if receipt_block is high
            # but the local _watermark was stale.
            receipt_block = int(receipt.get("receipt_block", 0))
            
            if receipt_block >= self._watermark:
                async with self._lock:
                    # Atomic increment of the total
                    new_total = self._current_total + float(receipt.get("grt_amount", 0))
                    
                    # Update DB with new total AND update the watermark
                    await self._db.update_one(
                        "tap_horizon_receipts",
                        {"sender_id": self._sender_id},
                        {"grt_total": new_total, "receipt_block": receipt_block}
                    )
                    
                    # 4. Refresh local state to avoid read-after-write lag
                    self._current_total = new_total
                    self._watermark = receipt_block
            elif receipt_block < self._watermark:
                # If receipt is older than watermark, it's likely a lag
                # Process it anyway but be careful with logic
                await self._process_old_receipt(receipt, self._watermark)

    async def _process_old_receipt(self, receipt, current_mark):
        """
        Handles receipt items that arrive 'out of order' or after a
        long idle period to prevent the tracker from freezing.
        """
        async with self._lock:
            await self._db.update_one(
                "tap_horizon_receipts",
                {"sender_id": self._sender_id, "receipt_block": current_mark},
                {"grt_total": self._current_total + float(receipt.get("grt_amount", 0))}
            )
            self._watermark = current_mark + 1

    async def get_fee_total(self):
        """
        Returns the current cached total, useful for external metrics
        like `tap_sender_fee_tracker_grt_total`.
        """
        async with self._lock:
            return self._current_total