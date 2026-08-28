use super::*;

impl Database {
    pub(crate) async fn set_user_item_played(
        &self,
        user_id: &str,
        item_id: &str,
        played: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, is_played, play_count, last_played_at)
             VALUES (?, ?, ?, CASE WHEN ? = 1 THEN 1 ELSE 0 END,
                     CASE WHEN ? = 1 THEN unixepoch() ELSE NULL END)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_played = excluded.is_played,
                 play_count = CASE
                     WHEN excluded.is_played = 1 AND user_item_state.is_played = 0
                     THEN user_item_state.play_count + 1 ELSE user_item_state.play_count END,
                 last_played_at = CASE
                     WHEN excluded.is_played = 1 THEN unixepoch()
                     ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_played != user_item_state.is_played THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(played))
        .bind(database_flag(played))
        .bind(database_flag(played))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_item_favorite(
        &self,
        user_id: &str,
        item_id: &str,
        favorite: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, is_favorite)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_favorite = excluded.is_favorite,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_favorite != user_item_state.is_favorite THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(favorite))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn record_playback_event(
        &self,
        event: NewPlaybackEvent<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let max_function = self.scalar_max_function();
        let auto_played = event.duration_ticks.is_some_and(|duration_ticks| {
            playback_reached_played_threshold(
                event.position_ticks,
                duration_ticks,
                event.played_percent,
            )
        });
        let playback_session_query = format!(
            "INSERT INTO playback_sessions (
                id, user_id, item_id, media_source_id, play_session_id,
                device_id, client, device_name, client_version, device_type,
                remote_ip, state,
                position_ticks, duration_ticks, is_paused
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, play_session_id) DO UPDATE SET
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                device_id = CASE
                    WHEN excluded.device_id = 'unknown' THEN playback_sessions.device_id
                    ELSE excluded.device_id END,
                client = COALESCE(excluded.client, playback_sessions.client),
                device_name = COALESCE(excluded.device_name, playback_sessions.device_name),
                client_version = COALESCE(excluded.client_version, playback_sessions.client_version),
                device_type = COALESCE(excluded.device_type, playback_sessions.device_type),
                remote_ip = COALESCE(excluded.remote_ip, playback_sessions.remote_ip),
                state = excluded.state,
                position_ticks = {max_function}(playback_sessions.position_ticks, excluded.position_ticks),
                duration_ticks = COALESCE(excluded.duration_ticks, playback_sessions.duration_ticks),
                is_paused = excluded.is_paused,
                last_event_at = unixepoch()"
        );
        self.query(sqlx::AssertSqlSafe(playback_session_query))
            .bind(Uuid::now_v7().to_string())
            .bind(event.user_id)
            .bind(event.item_id)
            .bind(event.media_source_id)
            .bind(event.play_session_id)
            .bind(event.device_id)
            .bind(event.client)
            .bind(event.device_name)
            .bind(event.client_version)
            .bind(event.device_type)
            .bind(event.remote_ip)
            .bind(event.state)
            .bind(event.position_ticks)
            .bind(event.duration_ticks)
            .bind(database_flag(event.is_paused))
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let user_item_state_query = format!(
            "INSERT INTO user_item_state (user_id, item_id, position_ticks, last_played_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 position_ticks = {max_function}(user_item_state.position_ticks, excluded.position_ticks),
                 last_played_at = CASE
                     WHEN excluded.position_ticks > user_item_state.position_ticks
                     THEN excluded.last_played_at ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.position_ticks > user_item_state.position_ticks THEN 1 ELSE 0 END"
        );
        self.query(sqlx::AssertSqlSafe(user_item_state_query))
            .bind(event.user_id)
            .bind(event.item_id)
            .bind(event.position_ticks)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if auto_played {
            self.query(
                "UPDATE user_item_state
                 SET is_played = 1,
                     play_count = CASE WHEN is_played = 0 THEN play_count + 1 ELSE play_count END,
                     last_played_at = unixepoch(),
                     version = version + CASE WHEN is_played = 0 THEN 1 ELSE 0 END
                 WHERE user_id = ? AND item_id = ?",
            )
            .bind(event.user_id)
            .bind(event.item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn sync_played_container_states(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let parent_ids: Vec<String> = self
            .query(
                "SELECT parent_id FROM media_items
                 WHERE id = ? AND item_type = 'EPISODE' AND parent_id IS NOT NULL
                 UNION
                 SELECT series_id FROM media_items
                 WHERE id = ? AND item_type = 'EPISODE' AND series_id IS NOT NULL",
            )
            .bind(item_id)
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect();
        if parent_ids.is_empty() {
            return Ok(());
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for parent_id in parent_ids {
            let is_played: i64 = self
                .query_scalar(
                    "WITH eligible AS (
                         SELECT episode.id
                         FROM media_items episode
                         JOIN media_items parent ON parent.id = ?
                         WHERE episode.item_type = 'EPISODE'
                           AND episode.removed_at IS NULL
                           AND episode.has_available_source = 1
                           AND ((parent.item_type = 'SEASON' AND episode.parent_id = parent.id)
                             OR (parent.item_type = 'SERIES' AND episode.series_id = parent.id))
                     )
                     SELECT CASE WHEN EXISTS (SELECT 1 FROM eligible)
                                      AND NOT EXISTS (
                                          SELECT 1
                                          FROM eligible
                                          LEFT JOIN user_item_state state
                                            ON state.user_id = ? AND state.item_id = eligible.id
                                          WHERE COALESCE(state.is_played, 0) = 0
                                      )
                                 THEN 1 ELSE 0 END",
                )
                .bind(&parent_id)
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query(
                "INSERT INTO user_item_state (user_id, item_id, is_played, play_count, last_played_at)
                 VALUES (?, ?, ?, CASE WHEN ? = 1 THEN 1 ELSE 0 END,
                         CASE WHEN ? = 1 THEN unixepoch() ELSE NULL END)
                 ON CONFLICT(user_id, item_id) DO UPDATE SET
                     is_played = excluded.is_played,
                     play_count = CASE
                         WHEN excluded.is_played = 1 AND user_item_state.is_played = 0
                         THEN user_item_state.play_count + 1 ELSE user_item_state.play_count END,
                     last_played_at = CASE
                         WHEN excluded.is_played = 1 THEN unixepoch()
                         ELSE user_item_state.last_played_at END,
                     version = user_item_state.version + CASE
                         WHEN excluded.is_played != user_item_state.is_played THEN 1 ELSE 0 END",
            )
            .bind(user_id)
            .bind(&parent_id)
            .bind(is_played)
            .bind(is_played)
            .bind(is_played)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_playback_sessions(
        &self,
        user_id: Option<&str>,
    ) -> Result<Vec<StoredPlaybackSession>, StorageError> {
        let (query, bind) = if user_id.is_some() {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE user_id = ?
                   AND state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id",
                user_id,
            )
        } else {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id",
                None,
            )
        };
        let mut statement = self.query(query);
        if let Some(user_id) = bind {
            statement = statement.bind(user_id);
        }
        statement = statement.bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS);
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(stored_playback_session).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_playback_session(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ? AND play_session_id = ?",
        )
        .bind(user_id)
        .bind(play_session_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_playback_session))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_active_playback_session(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ?
               AND item_id = ?
               AND state != 'STOPPED'
               AND last_event_at > unixepoch() - ?
             ORDER BY last_event_at DESC, id
             LIMIT 1",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_playback_session))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_web_playback_session(
        &self,
        session: NewWebPlaybackSession<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO web_playback_sessions (
                id, user_id, item_id, media_source_id, play_session_id,
                tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                last_sequence, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 'ACTIVE', ?, ?, ?, ?, -1, ?, ?)",
        )
        .bind(session.id)
        .bind(session.user_id)
        .bind(session.item_id)
        .bind(session.media_source_id)
        .bind(session.play_session_id)
        .bind(session.tier)
        .bind(session.plan)
        .bind(session.temp_dir)
        .bind(database_flag(session.is_admin))
        .bind(session.expires_at)
        .bind(session.now)
        .bind(session.now)
        .bind(session.now)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_web_playback_session(
        &self,
        session_id: &str,
    ) -> Result<Option<StoredWebPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                    last_sequence, created_at, updated_at
             FROM web_playback_sessions
             WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_web_playback_session))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn take_expired_web_playback_sessions(
        &self,
        now: i64,
    ) -> Result<Vec<StoredWebPlaybackSession>, StorageError> {
        let rows = self
            .query(
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                        last_sequence, created_at, updated_at
                 FROM web_playback_sessions
                 WHERE state = 'ACTIVE' AND expires_at < ?
                 ORDER BY expires_at ASC, id ASC
                 LIMIT 128",
            )
            .bind(now)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut expired = Vec::with_capacity(rows.len());
        for row in rows {
            let session = stored_web_playback_session(row);
            let updated = self
                .query(
                    "UPDATE web_playback_sessions
                     SET state = 'STOPPED', updated_at = ?
                     WHERE id = ? AND state = 'ACTIVE' AND expires_at < ?",
                )
                .bind(now)
                .bind(&session.id)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if updated.rows_affected() == 1 {
                expired.push(session);
            }
        }
        Ok(expired)
    }

    pub(crate) async fn set_web_playback_temp_dir(
        &self,
        session_id: &str,
        temp_dir: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET temp_dir = ?, updated_at = ?
             WHERE id = ? AND state = 'ACTIVE'",
        )
        .bind(temp_dir)
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn touch_web_playback_session(
        &self,
        session_id: &str,
        user_id: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET expires_at = ?, last_heartbeat_at = ?, updated_at = ?
             WHERE id = ? AND user_id = ? AND state = 'ACTIVE' AND expires_at >= ?",
        )
        .bind(expires_at)
        .bind(now)
        .bind(now)
        .bind(session_id)
        .bind(user_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn stop_web_playback_session(
        &self,
        session_id: &str,
        user_id: &str,
        state: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET state = ?, updated_at = ?
             WHERE id = ? AND user_id = ? AND state = 'ACTIVE'",
        )
        .bind(state)
        .bind(now)
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn accept_web_playback_event(
        &self,
        event: NewWebPlaybackEvent<'_>,
    ) -> Result<WebPlaybackEventClaim, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let inserted = self
            .query(
                "INSERT INTO web_playback_events (
                    session_id, event_id, sequence, state,
                    position_ticks, duration_ticks, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(event.session_id)
            .bind(event.event_id)
            .bind(event.sequence)
            .bind(event.state)
            .bind(event.position_ticks)
            .bind(event.duration_ticks)
            .bind(event.now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if inserted.rows_affected() == 0 {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(WebPlaybackEventClaim::Duplicate);
        }
        let updated = self
            .query(
                "UPDATE web_playback_sessions
                 SET state = CASE WHEN ? = 'STOPPED' THEN 'STOPPED' ELSE state END,
                     last_sequence = ?, updated_at = ?
                 WHERE id = ? AND user_id = ? AND state = 'ACTIVE'
                   AND expires_at >= ? AND last_sequence < ?",
            )
            .bind(event.state)
            .bind(event.sequence)
            .bind(event.now)
            .bind(event.session_id)
            .bind(event.user_id)
            .bind(event.now)
            .bind(event.sequence)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(if updated.rows_affected() == 1 {
            WebPlaybackEventClaim::Accepted
        } else {
            WebPlaybackEventClaim::Stale
        })
    }
}
