// Copyright 2025 OpenObserve Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::collections::HashMap as stdHashMap;

use async_trait::async_trait;
use config::{
    get_config,
    meta::stream::{
        FileKey, FileListDeleted, FileMeta, PartitionTimeLevel, StreamStats, StreamType,
    },
    utils::{
        parquet::parse_file_key_columns,
        time::{DAY_MICRO_SECS, end_of_the_day},
    },
};
use hashbrown::HashMap;
use sqlx::{Executor, Pool, QueryBuilder, Row, Sqlite};

use crate::{
    db::{
        IndexStatement,
        sqlite::{CLIENT_RO, CLIENT_RW, create_index},
    },
    errors::{Error, Result},
};

pub struct SqliteFileList {}

impl SqliteFileList {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for SqliteFileList {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl super::FileList for SqliteFileList {
    async fn create_table(&self) -> Result<()> {
        create_table().await
    }

    async fn create_table_index(&self) -> Result<()> {
        create_table_index().await
    }

    async fn add(&self, account: &str, file: &str, meta: &FileMeta) -> Result<i64> {
        self.inner_add("file_list", account, file, meta).await
    }

    async fn add_history(&self, account: &str, file: &str, meta: &FileMeta) -> Result<i64> {
        self.inner_add("file_list_history", account, file, meta)
            .await
    }

    async fn remove(&self, file: &str) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let pool = client.clone();
        let (stream_key, date_key, file_name) =
            parse_file_key_columns(file).map_err(|e| Error::Message(e.to_string()))?;

        sqlx::query(
            r#"UPDATE file_list SET deleted = true WHERE stream = $1 AND date = $2 AND file = $3;"#,
        )
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .execute(&pool)
        .await?;
        Ok(())
    }

    async fn batch_add(&self, files: &[FileKey]) -> Result<()> {
        self.inner_batch_process("file_list", files).await
    }

    async fn batch_add_with_id(&self, files: &[FileKey]) -> Result<()> {
        self.inner_batch_process("file_list", files).await
    }

    async fn batch_add_history(&self, files: &[FileKey]) -> Result<()> {
        self.inner_batch_process("file_list_history", files).await
    }

    async fn batch_process(&self, files: &[FileKey]) -> Result<()> {
        self.inner_batch_process("file_list", files).await
    }

    async fn update_dump_records(&self, dump_file: &FileKey, dumped_ids: &[i64]) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;

        let mut tx = client.begin().await?;

        let (stream_key, date_key, file_name) =
            parse_file_key_columns(&dump_file.key).map_err(|e| Error::Message(e.to_string()))?;
        let org_id = stream_key[..stream_key.find('/').unwrap()].to_string();
        let meta = &dump_file.meta;

        if let Err(e) = sqlx::query(r#" INSERT INTO file_list (account, org, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13);"#)
        .bind(&dump_file.account)
        .bind(org_id)
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .bind(false)
        .bind(meta.min_ts)
        .bind(meta.max_ts)
        .bind(meta.records)
        .bind(meta.original_size)
        .bind(meta.compressed_size)
        .bind(meta.index_size)
        .bind(meta.flattened)
        .execute(&mut *tx)
        .await{
            if let Err(e) = tx.rollback().await {
                log::error!("[SQLITE] rollback file_list dump file update error: {e}");
            }
            return Err(e.into());
        }

        for chunk in dumped_ids.chunks(get_config().limit.file_list_id_batch_size) {
            if chunk.is_empty() {
                continue;
            }
            let ids = chunk
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<String>>()
                .join(",");
            let query_str = format!("DELETE FROM file_list WHERE id IN ({ids})");
            if let Err(e) = sqlx::query(&query_str).execute(&mut *tx).await {
                if let Err(e) = tx.rollback().await {
                    log::error!("[SQLITE] rollback file_list dump file update error: {e}");
                }
                return Err(e.into());
            }
        }

        if let Err(e) = tx.commit().await {
            log::error!("[SQLITE] transaction commit error for dump file update {e}");
            return Err(e.into());
        }
        Ok(())
    }

    async fn batch_add_deleted(
        &self,
        org_id: &str,
        created_at: i64,
        files: &[FileListDeleted],
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let chunks = files.chunks(100);
        for files in chunks {
            // we don't care the id here, because the id is from file_list table not for this table
            let client = CLIENT_RW.clone();
            let client = client.lock().await;
            let mut tx = client.begin().await?;
            let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
                "INSERT INTO file_list_deleted (account, org, stream, date, file, index_file, flattened, created_at)",
            );
            query_builder.push_values(files, |mut b, item| {
                let (stream_key, date_key, file_name) =
                    parse_file_key_columns(&item.file).expect("parse file key failed");
                b.push_bind(&item.account)
                    .push_bind(org_id)
                    .push_bind(stream_key)
                    .push_bind(date_key)
                    .push_bind(file_name)
                    .push_bind(item.index_file)
                    .push_bind(item.flattened)
                    .push_bind(created_at);
            });
            if let Err(e) = query_builder.build().execute(&mut *tx).await {
                if let Err(e) = tx.rollback().await {
                    log::error!("[SQLITE] rollback file_list_deleted batch add error: {e}");
                }
                return Err(e.into());
            };
            if let Err(e) = tx.commit().await {
                log::error!("[SQLITE] commit file_list_deleted batch add error: {e}");
                return Err(e.into());
            }
        }
        Ok(())
    }

    async fn batch_remove_deleted(&self, files: &[FileKey]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let chunks = files.chunks(100);
        for files in chunks {
            // get ids of the files
            let client = CLIENT_RW.clone();
            let client = client.lock().await;
            let pool = client.clone();
            let mut ids = Vec::with_capacity(files.len());
            for file in files {
                if file.id > 0 {
                    ids.push(file.id.to_string());
                    continue;
                }
                let (stream_key, date_key, file_name) =
                    parse_file_key_columns(&file.key).map_err(|e| Error::Message(e.to_string()))?;
                let ret: Option<i64> = match sqlx::query_scalar(
                    r#"SELECT id FROM file_list_deleted WHERE stream = $1 AND date = $2 AND file = $3;"#,
                )
                .bind(stream_key)
                .bind(date_key)
                .bind(file_name)
                .fetch_one(&pool)
                .await
                {
                    Ok(v) => v,
                    Err(sqlx::Error::RowNotFound) => continue,
                    Err(e) => return Err(e.into()),
                };
                match ret {
                    Some(v) => ids.push(v.to_string()),
                    None => {
                        return Err(Error::Message(
                            "[SQLITE] query error: id should not empty from file_list_deleted"
                                .to_string(),
                        ));
                    }
                }
            }
            // delete files by ids
            if !ids.is_empty() {
                let sql = format!(
                    "DELETE FROM file_list_deleted WHERE id IN({});",
                    ids.join(",")
                );
                _ = pool.execute(sql.as_str()).await?;
            }
        }
        Ok(())
    }

    async fn get(&self, file: &str) -> Result<FileMeta> {
        let pool = CLIENT_RO.clone();
        let (stream_key, date_key, file_name) =
            parse_file_key_columns(file).map_err(|e| Error::Message(e.to_string()))?;
        let ret = sqlx::query_as::<_, super::FileRecord>(
            r#"
SELECT min_ts, max_ts, records, original_size, compressed_size, index_size, flattened
    FROM file_list WHERE stream = $1 AND date = $2 AND file = $3;
            "#,
        )
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .fetch_one(&pool)
        .await?;
        Ok(FileMeta::from(&ret))
    }

    async fn contains(&self, file: &str) -> Result<bool> {
        let pool = CLIENT_RO.clone();
        let (stream_key, date_key, file_name) =
            parse_file_key_columns(file).map_err(|e| Error::Message(e.to_string()))?;
        let ret = sqlx::query(
            r#"SELECT * FROM file_list WHERE stream = $1 AND date = $2 AND file = $3;"#,
        )
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .fetch_one(&pool)
        .await;
        if let Err(sqlx::Error::RowNotFound) = ret {
            return Ok(false);
        }
        Ok(!ret.unwrap().is_empty())
    }

    async fn update_flattened(&self, file: &str, flattened: bool) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let (stream_key, date_key, file_name) =
            parse_file_key_columns(file).map_err(|e| Error::Message(e.to_string()))?;
        sqlx::query(
            r#"UPDATE file_list SET flattened = $1 WHERE stream = $2 AND date = $3 AND file = $4;"#,
        )
        .bind(flattened)
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .execute(&*client)
        .await?;
        Ok(())
    }

    async fn update_compressed_size(&self, file: &str, size: i64) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let (stream_key, date_key, file_name) =
            parse_file_key_columns(file).map_err(|e| Error::Message(e.to_string()))?;
        sqlx::query(
            r#"UPDATE file_list SET compressed_size = $1 WHERE stream = $2 AND date = $3 AND file = $4;"#,
        )
        .bind(size)
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .execute(&*client)
        .await?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<FileKey>> {
        let pool = CLIENT_RO.clone();
        let ret = sqlx::query_as::<_, super::FileRecord>(
            r#"SELECT id, account, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened FROM file_list;"#,
        )
        .fetch_all(&pool)
        .await?;
        Ok(ret.iter().map(|r| r.into()).collect())
    }

    async fn query(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream_name: &str,
        _time_level: PartitionTimeLevel,
        time_range: Option<(i64, i64)>,
        flattened: Option<bool>,
    ) -> Result<Vec<FileKey>> {
        if let Some((start, end)) = time_range
            && start == 0
            && end == 0
        {
            return Ok(Vec::new());
        }

        let stream_key = format!("{org_id}/{stream_type}/{stream_name}");

        let pool = CLIENT_RO.clone();
        let ret = if flattened.is_some() {
            sqlx::query_as::<_, super::FileRecord>(
                r#"
SELECT id, account, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened
    FROM file_list
    WHERE stream = $1 AND flattened = $2 LIMIT 1000;
                "#,
            )
            .bind(stream_key)
            .bind(flattened.unwrap())
            .fetch_all(&pool)
            .await
        } else {
            let (time_start, time_end) = time_range.unwrap_or((0, 0));
            let max_ts_upper_bound = super::calculate_max_ts_upper_bound(time_end, stream_type);
            sqlx::query_as::<_, super::FileRecord>(
                r#"
SELECT id, account, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened
    FROM file_list
    WHERE stream = $1 AND max_ts >= $2 AND max_ts <= $3 AND min_ts <= $4;
                "#,
            )
            .bind(stream_key)
            .bind(time_start)
            .bind(max_ts_upper_bound)
            .bind(time_end)
            .fetch_all(&pool)
            .await
        };
        Ok(ret?
            .iter()
            .filter_map(|r| if r.deleted { None } else { Some(r.into()) })
            .collect())
    }

    async fn query_for_merge(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream_name: &str,
        date_range: Option<(String, String)>,
    ) -> Result<Vec<FileKey>> {
        if let Some((start, end)) = date_range.as_ref()
            && start.is_empty()
            && end.is_empty()
        {
            return Ok(Vec::new());
        }

        let stream_key = format!("{org_id}/{stream_type}/{stream_name}");

        let pool = CLIENT_RO.clone();
        let (date_start, date_end) = date_range.unwrap_or(("".to_string(), "".to_string()));
        let ret = sqlx::query_as::<_, super::FileRecord>(
                r#"
SELECT id, account, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened
    FROM file_list
    WHERE stream = $1 AND date >= $2 AND date <= $3;
                "#,
            )
            .bind(stream_key)
            .bind(date_start)
            .bind(date_end)
            .fetch_all(&pool)
            .await;
        Ok(ret?
            .iter()
            .filter_map(|r| if r.deleted { None } else { Some(r.into()) })
            .collect())
    }

    async fn query_by_ids(&self, ids: &[i64]) -> Result<Vec<FileKey>> {
        if ids.is_empty() {
            return Ok(Vec::default());
        }

        let mut ret = Vec::new();
        let pool = CLIENT_RO.clone();

        for chunk in ids.chunks(get_config().limit.file_list_id_batch_size) {
            if chunk.is_empty() {
                continue;
            }
            let ids = chunk
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<String>>()
                .join(",");
            let query_str = format!(
                "SELECT id, account, stream, date, file, min_ts, max_ts, records, original_size, compressed_size, index_size FROM file_list WHERE id IN ({ids})"
            );
            let res = sqlx::query_as::<_, super::FileRecord>(&query_str)
                .fetch_all(&pool)
                .await?;
            ret.extend_from_slice(&res);
        }

        Ok(ret.iter().map(|r| r.into()).collect())
    }

    async fn query_ids(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream_name: &str,
        time_range: Option<(i64, i64)>,
    ) -> Result<Vec<super::FileId>> {
        if let Some((start, end)) = time_range
            && start == 0
            && end == 0
        {
            return Ok(Vec::new());
        }

        let stream_key = format!("{org_id}/{stream_type}/{stream_name}");
        let (time_start, time_end) = time_range.unwrap_or((0, 0));

        let day_partitions = if time_end - time_start <= DAY_MICRO_SECS
            || time_end - time_start > DAY_MICRO_SECS * 30
            || !get_config().limit.file_list_multi_thread
        {
            vec![(time_start, time_end)]
        } else {
            let mut partitions = Vec::new();
            let mut start = time_start;
            while start < time_end {
                let end_of_day = std::cmp::min(end_of_the_day(start), time_end);
                partitions.push((start, end_of_day));
                start = end_of_day + 1; // next day, use end_of_day + 1 microsecond
            }
            partitions
        };
        log::debug!("file_list day_partitions: {:?}", day_partitions);

        let mut tasks = Vec::with_capacity(day_partitions.len());

        for (time_start, time_end) in day_partitions {
            let stream_key = stream_key.clone();
            tasks.push(tokio::task::spawn(async move {
                let pool = CLIENT_RO.clone();
                    let max_ts_upper_bound = super::calculate_max_ts_upper_bound(time_end, stream_type);
                    let query = "SELECT id, records, original_size, deleted FROM file_list WHERE stream = $1 AND max_ts >= $2 AND max_ts <= $3 AND min_ts <= $4;";
                    sqlx::query_as::<_, super::FileId>(query)
                    .bind(stream_key)
                    .bind(time_start)
                    .bind(max_ts_upper_bound)
                    .bind(time_end)
                    .fetch_all(&pool)
                    .await
            }));
        }

        let mut rets = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(r)) => rets.extend(r.into_iter().filter(|r| !r.deleted)),
                Ok(Err(e)) => {
                    return Err(e.into());
                }
                Err(e) => {
                    return Err(Error::Message(e.to_string()));
                }
            };
        }
        Ok(rets)
    }

    async fn query_ids_by_files(&self, files: &[FileKey]) -> Result<stdHashMap<String, i64>> {
        let mut ret = stdHashMap::with_capacity(files.len());
        // group by date
        let mut stream_files = HashMap::new();
        let mut files_map = HashMap::with_capacity(files.len());
        for file in files {
            if file.id > 0 {
                ret.insert(file.key.clone(), file.id);
                continue;
            }
            let (stream_key, date_key, file_name) =
                parse_file_key_columns(&file.key).map_err(|e| Error::Message(e.to_string()))?;
            let stream_entry = stream_files.entry(stream_key).or_insert(HashMap::new());
            let date_entry = stream_entry.entry(date_key).or_insert(Vec::new());
            date_entry.push(file_name.clone());
            files_map.insert(file_name, &file.key);
        }
        for (stream_key, stream_files) in stream_files {
            let pool = CLIENT_RO.clone();
            for (date_key, files) in stream_files {
                if files.is_empty() {
                    continue;
                }
                let sql = format!(
                    "SELECT id, file FROM file_list WHERE stream = $1 AND date = $2 AND file IN ('{}');",
                    files.join("','")
                );
                let query_res = sqlx::query_as::<_, super::FileIdWithFile>(&sql)
                    .bind(&stream_key)
                    .bind(&date_key)
                    .fetch_all(&pool)
                    .await?;
                for file in query_res {
                    if let Some(file_name) = files_map.get(&file.file) {
                        ret.insert(file_name.to_string(), file.id);
                    }
                }
            }
        }
        Ok(ret)
    }

    async fn query_old_data_hours(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream_name: &str,
        time_range: Option<(i64, i64)>,
    ) -> Result<Vec<String>> {
        if let Some((start, end)) = time_range
            && start == 0
            && end == 0
        {
            return Ok(Vec::new());
        }

        let stream_key = format!("{org_id}/{stream_type}/{stream_name}");

        let pool = CLIENT_RO.clone();
        let (time_start, time_end) = time_range.unwrap_or((0, 0));
        let cfg = get_config();
        let max_ts_upper_bound = super::calculate_max_ts_upper_bound(time_end, stream_type);
        let sql = r#"
SELECT date
    FROM file_list
    WHERE stream = $1 AND max_ts >= $2 AND max_ts <= $3 AND min_ts <= $4 AND records < $5
    GROUP BY date HAVING count(*) >= $6;
            "#;

        let ret = sqlx::query(sql)
            .bind(stream_key)
            .bind(time_start)
            .bind(max_ts_upper_bound)
            .bind(time_end)
            .bind(cfg.compact.old_data_min_records)
            .bind(cfg.compact.old_data_min_files)
            .fetch_all(&pool)
            .await?;
        Ok(ret
            .into_iter()
            .map(|r| r.try_get::<String, &str>("date").unwrap_or_default())
            .collect())
    }

    async fn query_deleted(
        &self,
        org_id: &str,
        time_max: i64,
        limit: i64,
    ) -> Result<Vec<FileListDeleted>> {
        if time_max == 0 {
            return Ok(Vec::new());
        }
        let pool = CLIENT_RO.clone();
        let ret = sqlx::query_as::<_, super::FileDeletedRecord>(
            r#"SELECT id, account, stream, date, file, index_file, flattened FROM file_list_deleted WHERE org = $1 AND created_at < $2 ORDER BY created_at ASC LIMIT $3;"#,
        )
        .bind(org_id)
        .bind(time_max)
        .bind(limit)
        .fetch_all(&pool)
        .await?;
        Ok(ret
            .iter()
            .map(|r| FileListDeleted {
                id: r.id,
                account: r.account.to_string(),
                file: format!("files/{}/{}/{}", r.stream, r.date, r.file),
                index_file: r.index_file,
                flattened: r.flattened,
            })
            .collect())
    }

    async fn list_deleted(&self) -> Result<Vec<FileListDeleted>> {
        let pool = CLIENT_RO.clone();
        let ret = sqlx::query_as::<_, super::FileDeletedRecord>(
            r#"SELECT id, account, stream, date, file, index_file, flattened FROM file_list_deleted;"#,
        )
        .fetch_all(&pool)
        .await?;
        Ok(ret
            .iter()
            .map(|r| FileListDeleted {
                id: r.id,
                account: r.account.to_string(),
                file: format!("files/{}/{}/{}", r.stream, r.date, r.file),
                index_file: r.index_file,
                flattened: r.flattened,
            })
            .collect())
    }

    async fn get_min_ts(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream_name: &str,
    ) -> Result<i64> {
        let stream_key = format!("{org_id}/{stream_type}/{stream_name}");
        let min_ts = config::utils::time::BASE_TIME.timestamp_micros();
        let pool = CLIENT_RO.clone();
        let ret: Option<i64> = sqlx::query_scalar(
            r#"SELECT MIN(min_ts) AS id FROM file_list WHERE stream = $1 AND min_ts > $2;"#,
        )
        .bind(stream_key)
        .bind(min_ts)
        .fetch_one(&pool)
        .await?;
        Ok(ret.unwrap_or_default())
    }

    async fn get_max_pk_value(&self) -> Result<i64> {
        let pool = CLIENT_RO.clone();
        let ret: Option<i64> = sqlx::query_scalar(r#"SELECT MAX(id) AS id FROM file_list;"#)
            .fetch_one(&pool)
            .await?;
        Ok(ret.unwrap_or_default())
    }

    async fn get_min_pk_value(&self) -> Result<i64> {
        let pool = CLIENT_RO.clone();
        let ret: Option<i64> = sqlx::query_scalar(r#"SELECT MIN(id) AS id FROM file_list;"#)
            .fetch_one(&pool)
            .await?;
        Ok(ret.unwrap_or_default())
    }

    async fn clean_by_min_pk_value(&self, val: i64) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        sqlx::query("DELETE FROM file_list WHERE id < $1;")
            .bind(val)
            .execute(&*client)
            .await?;
        Ok(())
    }

    async fn stats(
        &self,
        org_id: &str,
        stream_type: Option<StreamType>,
        stream_name: Option<&str>,
        pk_value: Option<(i64, i64)>,
        deleted: bool,
    ) -> Result<Vec<(String, StreamStats)>> {
        let (field, value) = if stream_type.is_some() && stream_name.is_some() {
            (
                "stream",
                format!(
                    "{}/{}/{}",
                    org_id,
                    stream_type.unwrap(),
                    stream_name.unwrap()
                ),
            )
        } else {
            ("org", org_id.to_string())
        };
        let file_list_stream = format!("{org_id}/file_list/");
        let mut sql = format!(
            r#"
SELECT stream, MIN(min_ts) as min_ts, MAX(max_ts) as max_ts, COUNT(*) as file_num, SUM(records) as records, SUM(original_size) as original_size, SUM(compressed_size) as compressed_size, SUM(index_size) as index_size
    FROM file_list
    WHERE {field} = '{value}' AND stream NOT LIKE '{file_list_stream}%'
            "#,
        );
        if deleted {
            sql = format!("{sql} AND deleted IS TRUE");
        }
        let sql = match pk_value {
            None => format!("{sql} GROUP BY stream"),
            Some((0, 0)) => format!("{sql} GROUP BY stream"),
            Some((min, max)) => {
                if deleted {
                    format!("{sql} AND id <= {max} GROUP BY stream")
                } else {
                    format!("{sql} AND id > {min} AND id <= {max} GROUP BY stream")
                }
            }
        };
        let pool = CLIENT_RO.clone();
        let ret = sqlx::query_as::<_, super::StatsRecord>(&sql)
            .fetch_all(&pool)
            .await?;
        Ok(ret
            .iter()
            .map(|r| (r.stream.to_owned(), r.into()))
            .collect())
    }

    async fn get_stream_stats(
        &self,
        org_id: &str,
        stream_type: Option<StreamType>,
        stream_name: Option<&str>,
    ) -> Result<Vec<(String, StreamStats)>> {
        let sql = if stream_type.is_some() && stream_name.is_some() {
            format!(
                "SELECT * FROM stream_stats WHERE stream = '{org_id}/{}/{}';",
                stream_type.unwrap(),
                stream_name.unwrap()
            )
        } else {
            format!("SELECT * FROM stream_stats WHERE org = '{org_id}';")
        };
        let pool = CLIENT_RO.clone();
        let ret = sqlx::query_as::<_, super::StatsRecord>(&sql)
            .fetch_all(&pool)
            .await?;
        Ok(ret
            .iter()
            .map(|r| (r.stream.to_owned(), r.into()))
            .collect())
    }

    async fn del_stream_stats(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream_name: &str,
    ) -> Result<()> {
        let sql = format!(
            "DELETE FROM stream_stats WHERE stream = '{org_id}/{stream_type}/{stream_name}';"
        );
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        sqlx::query(&sql).execute(&*client).await?;
        Ok(())
    }

    async fn set_stream_stats(
        &self,
        org_id: &str,
        streams: &[(String, StreamStats)],
        pk_value: Option<(i64, i64)>,
    ) -> Result<()> {
        let old_stats = super::get_stream_stats(org_id, None, None).await?;
        let old_stats = old_stats.into_iter().collect::<HashMap<_, _>>();
        let mut new_streams = Vec::new();
        let mut update_streams = Vec::with_capacity(streams.len());
        for (stream_key, item) in streams {
            let mut stats = match old_stats.get(stream_key) {
                Some(s) => s.to_owned(),
                None => {
                    new_streams.push(stream_key);
                    StreamStats::default()
                }
            };
            stats.format_by(item); // format stats
            update_streams.push((stream_key, stats));
        }

        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let mut tx = client.begin().await?;
        for stream_key in new_streams {
            let org_id = stream_key[..stream_key.find('/').unwrap()].to_string();
            if let Err(e) = sqlx::query(
                r#"
    INSERT INTO stream_stats
    (org, stream, file_num, min_ts, max_ts, records, original_size, compressed_size, index_size)
    VALUES ($1, $2, 0, 0, 0, 0, 0, 0, 0);
                "#,
            )
            .bind(org_id)
            .bind(stream_key)
            .execute(&mut *tx)
            .await
            {
                if let Err(e) = tx.rollback().await {
                    log::error!("[SQLITE] rollback insert stream stats error: {e}");
                }
                return Err(e.into());
            }
        }
        if let Err(e) = tx.commit().await {
            log::error!("[SQLITE] commit set stream stats error: {e}");
            return Err(e.into());
        }

        let mut tx = client.begin().await?;
        // update stats
        for (stream_key, stats) in update_streams {
            if let Err(e) = sqlx::query(
                r#"
UPDATE stream_stats
    SET file_num = file_num + $1, min_ts = $2, max_ts = $3, records = records + $4, original_size = original_size + $5, compressed_size = compressed_size + $6, index_size = index_size + $7
    WHERE stream = $8;
                "#,
            )
            .bind(stats.file_num)
            .bind(stats.doc_time_min)
            .bind(stats.doc_time_max)
            .bind(stats.doc_num)
            .bind(stats.storage_size as i64)
            .bind(stats.compressed_size as i64)
            .bind(stats.index_size as i64)
            .bind(stream_key)
            .execute(&mut *tx)
            .await
            {
                if let Err(e) = tx.rollback().await {
                    log::error!("[SQLITE] rollback set stream stats error: {e}");
                }
                return Err(e.into());
            }
        }

        // delete files which already marked deleted
        if let Some((_min_id, max_id)) = pk_value {
            let limit = config::get_config().limit.calculate_stats_step_limit;
            loop {
                let start = std::time::Instant::now();
                match sqlx::query("DELETE FROM file_list WHERE id IN (SELECT id FROM file_list WHERE org = $1 AND deleted IS TRUE AND id <= $2 LIMIT $3);")
                    .bind(org_id)
                    .bind(max_id)
                    .bind(limit)
                    .execute(&mut *tx)
                    .await
                {
                    Ok(v) => {
                        log::debug!("[SQLITE] delete file list rows affected: {}, took: {} ms", v.rows_affected(), start.elapsed().as_millis());
                        if v.rows_affected() == 0 {
                            break;
                        }
                    }
                    Err(e) => {
                        if let Err(e) = tx.rollback().await {
                            log::error!(
                                "[SQLITE] rollback set stream stats error for delete file list: {e}"
                            );
                        }
                        return Err(e.into());
                    }
                }
            }
        }

        // commit
        if let Err(e) = tx.commit().await {
            log::error!("[SQLITE] commit set stream stats error: {e}");
            return Err(e.into());
        }

        Ok(())
    }

    async fn reset_stream_stats_min_ts(
        &self,
        _org_id: &str,
        stream: &str,
        min_ts: i64,
    ) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        sqlx::query(r#"UPDATE stream_stats SET min_ts = $1 WHERE stream = $2;"#)
            .bind(min_ts)
            .bind(stream)
            .execute(&*client)
            .await?;
        sqlx::query(
            r#"UPDATE stream_stats SET max_ts = min_ts WHERE stream = $1 AND max_ts < min_ts;"#,
        )
        .bind(stream)
        .execute(&*client)
        .await?;
        Ok(())
    }

    async fn reset_stream_stats(&self) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        sqlx::query(r#"UPDATE stream_stats SET file_num = 0, min_ts = 0, max_ts = 0, records = 0, original_size = 0, compressed_size = 0, index_size = 0;"#)
        .execute(&*client)
       .await?;
        Ok(())
    }

    async fn len(&self) -> usize {
        let pool = CLIENT_RO.clone();
        let ret = match sqlx::query(r#"SELECT COUNT(*) as num FROM file_list;"#)
            .fetch_one(&pool)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                log::error!("[SQLITE] get file list len error: {e}");
                return 0;
            }
        };
        match ret.try_get::<i64, &str>("num") {
            Ok(v) => v as usize,
            _ => 0,
        }
    }

    async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    async fn clear(&self) -> Result<()> {
        Ok(())
    }

    async fn add_job(
        &self,
        org_id: &str,
        stream_type: StreamType,
        stream: &str,
        offset: i64,
    ) -> Result<i64> {
        let stream_key = format!("{org_id}/{stream_type}/{stream}");
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let mut tx = client.begin().await?;
        match sqlx::query(
            "INSERT INTO file_list_jobs (org, stream, offsets, status, node, started_at, updated_at) VALUES ($1, $2, $3, $4, '', 0, 0);",
        )
        .bind(org_id)
        .bind(&stream_key)
        .bind(offset)
        .bind(super::FileListJobStatus::Pending)
        .execute(&mut *tx)
        .await
        {
            Err(sqlx::Error::Database(e)) => if !e.is_unique_violation() {
                return Err(Error::Message(e.to_string()));
            },
            Err(e) => {
                return Err(e.into());
            },
            Ok(_) => {}
        };

        // get job id
        let ret = match sqlx::query(
            "SELECT id, status FROM file_list_jobs WHERE org = $1 AND stream = $2 AND offsets = $3;",
        )
        .bind(org_id)
        .bind(&stream_key)
        .bind(offset)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                if let Err(e) = tx.rollback().await {
                    log::error!("[SQLITE] rollback add job error: {e}");
                }
                return Err(e.into());
            }
        };
        let id = ret.try_get::<i64, &str>("id").unwrap_or_default();
        let status = ret.try_get::<i64, &str>("status").unwrap_or_default();
        if id > 0
            && super::FileListJobStatus::from(status) == super::FileListJobStatus::Done
            && let Err(e) =
                sqlx::query("UPDATE file_list_jobs SET status = $1 WHERE status = $2 AND id = $3;")
                    .bind(super::FileListJobStatus::Pending)
                    .bind(super::FileListJobStatus::Done)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
        {
            if let Err(e) = tx.rollback().await {
                log::error!("[SQLITE] rollback update job status error: {e}");
            }
            return Err(e.into());
        }
        if let Err(e) = tx.commit().await {
            log::error!("[SQLITE] commit add job error: {e}");
            return Err(e.into());
        }
        Ok(id)
    }

    async fn get_pending_jobs(&self, node: &str, limit: i64) -> Result<Vec<super::MergeJobRecord>> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let mut tx = client.begin().await?;
        // get pending jobs group by stream and order by num desc
        let ret = match sqlx::query_as::<_, super::MergeJobPendingRecord>(
            r#"
SELECT stream, max(id) as id, COUNT(*) AS num
    FROM file_list_jobs
    WHERE status = $1
    GROUP BY stream
    ORDER BY num DESC
    LIMIT $2;"#,
        )
        .bind(super::FileListJobStatus::Pending)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                if let Err(e) = tx.rollback().await {
                    log::error!(
                        "[SQLITE] rollback select file_list_jobs pending jobs for update error: {e}"
                    );
                }
                return Err(e.into());
            }
        };
        // update jobs status to running
        let ids = ret.iter().map(|r| r.id.to_string()).collect::<Vec<_>>();
        if ids.is_empty() {
            if let Err(e) = tx.rollback().await {
                log::error!("[SQLITE] rollback select file_list_jobs pending jobs error: {e}");
            }
            return Ok(Vec::new());
        }
        let sql = format!(
            "UPDATE file_list_jobs SET status = $1, node = $2, started_at = $3, updated_at = $4 WHERE id IN ({});",
            ids.join(",")
        );
        let now = config::utils::time::now_micros();
        if let Err(e) = sqlx::query(&sql)
            .bind(super::FileListJobStatus::Running)
            .bind(node)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await
        {
            if let Err(e) = tx.rollback().await {
                log::error!("[SQLITE] rollback update file_list_jobs status error: {e}");
            }
            return Err(e.into());
        }
        // get jobs by ids
        let sql = format!(
            "SELECT * FROM file_list_jobs WHERE id IN ({});",
            ids.join(",")
        );
        let ret = match sqlx::query_as::<_, super::MergeJobRecord>(&sql)
            .fetch_all(&mut *tx)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                if let Err(e) = tx.rollback().await {
                    log::error!("[SQLITE] rollback select file_list_jobs by ids error: {e}");
                }
                return Err(e.into());
            }
        };
        if let Err(e) = tx.commit().await {
            log::error!("[SQLITE] commit select file_list_jobs pending jobs error: {e}");
            return Err(e.into());
        }
        Ok(ret)
    }

    async fn set_job_pending(&self, ids: &[i64]) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let sql = format!(
            "UPDATE file_list_jobs SET status = $1 WHERE id IN ({});",
            ids.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        sqlx::query(&sql)
            .bind(super::FileListJobStatus::Pending)
            .execute(&*client)
            .await?;
        Ok(())
    }

    async fn set_job_done(&self, ids: &[i64]) -> Result<()> {
        let config = get_config();
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let sql = format!(
            "UPDATE file_list_jobs SET status = $1, updated_at = $2, dumped = $3 WHERE id IN ({});",
            ids.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        // if dump enabled we set dumped to false, so dump job can work
        // id dump disabled, we set it to true, so cleanup can remvoe the jobs
        sqlx::query(&sql)
            .bind(super::FileListJobStatus::Done)
            .bind(config::utils::time::now_micros())
            .bind(!config.common.file_list_dump_enabled)
            .execute(&*client)
            .await?;
        Ok(())
    }

    async fn update_running_jobs(&self, ids: &[i64]) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let sql = format!(
            r#"UPDATE file_list_jobs SET updated_at = $1 WHERE id IN ({})"#,
            ids.iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        sqlx::query(&sql)
            .bind(config::utils::time::now_micros())
            .execute(&*client)
            .await?;
        Ok(())
    }

    async fn check_running_jobs(&self, before_date: i64) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let ret = sqlx::query(
            r#"UPDATE file_list_jobs SET status = $1 WHERE status = $2 AND updated_at < $3;"#,
        )
        .bind(super::FileListJobStatus::Pending)
        .bind(super::FileListJobStatus::Running)
        .bind(before_date)
        .execute(&*client)
        .await?;
        if ret.rows_affected() > 0 {
            log::warn!("[SQLITE] reset running jobs status to pending");
        }
        Ok(())
    }

    async fn clean_done_jobs(&self, before_date: i64) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let ret = sqlx::query(
            r#"DELETE FROM file_list_jobs WHERE status = $1 AND updated_at < $2 AND dumped = $3;"#,
        )
        .bind(super::FileListJobStatus::Done)
        .bind(before_date)
        .bind(true)
        .execute(&*client)
        .await?;
        if ret.rows_affected() > 0 {
            log::warn!("[SQLITE] clean done jobs");
        }
        Ok(())
    }

    async fn get_pending_jobs_count(&self) -> Result<stdHashMap<String, stdHashMap<String, i64>>> {
        let pool = CLIENT_RO.clone();

        let ret =
            sqlx::query(r#"SELECT stream, status, count(*) as counts FROM file_list_jobs WHERE status = $1 GROUP BY stream, status ORDER BY status desc;"#)
                .bind(super::FileListJobStatus::Pending)
                .fetch_all(&pool)
                .await?;

        let mut job_status: stdHashMap<String, stdHashMap<String, i64>> = stdHashMap::new();

        for r in ret.iter() {
            let stream = r.get::<String, &str>("stream");
            let status = r.get::<i32, &str>("status");
            let counts = if status == 0 {
                r.get::<i64, &str>("counts")
            } else {
                0
            };
            let parts: Vec<&str> = stream.split('/').collect();
            if parts.len() >= 2 {
                let org = parts[0].to_string();
                let stream_type = parts[1].to_string();
                job_status
                    .entry(org)
                    .or_default()
                    .entry(stream_type)
                    .and_modify(|e| *e = counts)
                    .or_insert(counts);
            }
        }
        Ok(job_status)
    }

    async fn get_entries_in_range(
        &self,
        org: &str,
        stream: Option<&str>,
        time_start: i64,
        time_end: i64,
        min_id: Option<i64>,
    ) -> Result<Vec<super::FileRecord>> {
        if time_start == 0 && time_end == 0 {
            return Ok(Vec::new());
        }

        let day_partitions = if time_end - time_start <= DAY_MICRO_SECS
            || time_end - time_start > DAY_MICRO_SECS * 30
            || !get_config().limit.file_list_multi_thread
        {
            vec![(time_start, time_end)]
        } else {
            let mut partitions = Vec::new();
            let mut start = time_start;
            while start < time_end {
                let end_of_day = std::cmp::min(end_of_the_day(start), time_end);
                partitions.push((start, end_of_day));
                start = end_of_day + 1; // next day, use end_of_day + 1 microsecond
            }
            partitions
        };

        let mut tasks = Vec::with_capacity(day_partitions.len());

        for (time_start, time_end) in day_partitions {
            let o = org.to_string();
            let sql = "SELECT * FROM file_list WHERE max_ts >= $1 AND min_ts <= $2 AND org = $3 AND deleted = $4";
            let sql = match stream {
                Some(stream) => format!("{sql} AND stream = '{stream}'"),
                None => sql.to_string(),
            };
            let sql = match min_id {
                Some(id) => format!("{sql} AND id >= {id}"),
                None => sql,
            };
            tasks.push(tokio::task::spawn(async move {
                let pool = CLIENT_RO.clone();
                sqlx::query_as::<_, super::FileRecord>(&sql)
                    .bind(time_start)
                    .bind(time_end)
                    .bind(o)
                    .bind(false)
                    .fetch_all(&pool)
                    .await
            }));
        }

        let mut rets = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(r)) => rets.extend(r.into_iter().filter(|r| !r.deleted)),
                Ok(Err(e)) => {
                    return Err(e.into());
                }
                Err(e) => {
                    return Err(Error::Message(e.to_string()));
                }
            };
        }
        Ok(rets)
    }

    async fn get_pending_dump_jobs(&self) -> Result<Vec<(i64, String, String, i64)>> {
        let pool = CLIENT_RO.clone();

        let ret = sqlx::query_as::<_, (i64,String, String, i64)>(
            r#"SELECT id, org, stream, offsets FROM file_list_jobs WHERE status = $1 AND dumped = $2 limit 1000"#,
        )
        .bind(super::FileListJobStatus::Done)
        .bind(false)
        .fetch_all(&pool)
        .await?;

        let mut pending: Vec<(i64, String, String, i64)> = Vec::new();

        for (id, org, stream, offset) in ret.iter() {
            pending.push((*id, org.to_string(), stream.to_string(), *offset));
        }

        Ok(pending)
    }
    async fn set_job_dumped_status(&self, id: i64, dumped: bool) -> Result<()> {
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        sqlx::query("UPDATE file_list_jobs SET dumped = $1 WHERE id = $2;")
            .bind(dumped)
            .bind(id)
            .execute(&*client)
            .await?;
        Ok(())
    }
}

impl SqliteFileList {
    async fn inner_add(
        &self,
        table: &str,
        account: &str,
        file: &str,
        meta: &FileMeta,
    ) -> Result<i64> {
        self.inner_add_with_id(table, None, account, file, meta)
            .await
    }

    async fn inner_add_with_id(
        &self,
        table: &str,
        id: Option<i64>,
        account: &str,
        file: &str,
        meta: &FileMeta,
    ) -> Result<i64> {
        let (stream_key, date_key, file_name) =
            parse_file_key_columns(file).map_err(|e| Error::Message(e.to_string()))?;
        let org_id = stream_key[..stream_key.find('/').unwrap()].to_string();
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        match  sqlx::query(
            format!(r#"
INSERT INTO {table} (id, account, org, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14);
        "#).as_str(),
    )
        .bind(id)
        .bind(account)
        .bind(org_id)
        .bind(stream_key)
        .bind(date_key)
        .bind(file_name)
        .bind(false)
        .bind(meta.min_ts)
        .bind(meta.max_ts)
        .bind(meta.records)
        .bind(meta.original_size)
        .bind(meta.compressed_size)
        .bind(meta.index_size)
        .bind(meta.flattened)
        .execute(&*client)
        .await {
            Err(sqlx::Error::Database(e)) => if e.is_unique_violation() {
                  Ok(0)
            } else {
                  Err(Error::Message(e.to_string()))
            },
            Err(e) =>  Err(e.into()),
            Ok(v) => Ok(v.last_insert_rowid()),
        }
    }

    async fn inner_batch_process(&self, table: &str, files: &[FileKey]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }

        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let mut tx = client.begin().await?;

        let add_items = files.iter().filter(|f| !f.deleted).collect::<Vec<_>>();
        if !add_items.is_empty() {
            let chunks = add_items.chunks(100);
            for files in chunks {
                let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
                format!("INSERT INTO {table} (id, account, org, stream, date, file, deleted, min_ts, max_ts, records, original_size, compressed_size, index_size, flattened)").as_str(),
                );
                query_builder.push_values(files, |mut b, item| {
                    let id = if item.id > 0 { Some(item.id) } else { None };
                    let (stream_key, date_key, file_name) =
                        parse_file_key_columns(&item.key).expect("parse file key failed");
                    let org_id = stream_key[..stream_key.find('/').unwrap()].to_string();
                    b.push_bind(id)
                        .push_bind(&item.account)
                        .push_bind(org_id)
                        .push_bind(stream_key)
                        .push_bind(date_key)
                        .push_bind(file_name)
                        .push_bind(false)
                        .push_bind(item.meta.min_ts)
                        .push_bind(item.meta.max_ts)
                        .push_bind(item.meta.records)
                        .push_bind(item.meta.original_size)
                        .push_bind(item.meta.compressed_size)
                        .push_bind(item.meta.index_size)
                        .push_bind(item.meta.flattened);
                });
                query_builder.push(" ON CONFLICT(id) DO NOTHING");
                if let Err(e) = query_builder.build().execute(&mut *tx).await {
                    if let Err(e) = tx.rollback().await {
                        log::error!("[SQLITE] rollback {table} batch process for add error: {e}");
                    }
                    return Err(e.into());
                }
            }
        }

        let del_items = files.iter().filter(|f| f.deleted).collect::<Vec<_>>();
        if !del_items.is_empty() {
            let chunks = del_items.chunks(1000);
            for files in chunks {
                // get ids of the files
                let mut ids = Vec::with_capacity(files.len());
                for file in files {
                    if file.id > 0 {
                        ids.push(file.id.to_string());
                        continue;
                    }
                    let (stream_key, date_key, file_name) = parse_file_key_columns(&file.key)
                        .map_err(|e| Error::Message(e.to_string()))?;
                    let query_res: std::result::Result<Option<i64>, sea_orm::SqlxError> = sqlx::query_scalar(
                    r#"SELECT id FROM file_list WHERE stream = $1 AND date = $2 AND file = $3;"#,
                    )
                    .bind(stream_key)
                    .bind(date_key)
                    .bind(file_name)
                    .fetch_one(&mut *tx)
                    .await;
                    match query_res {
                        Ok(Some(v)) => ids.push(v.to_string()),
                        Ok(None) => continue,
                        Err(sqlx::Error::RowNotFound) => continue,
                        Err(e) => {
                            if let Err(e) = tx.rollback().await {
                                log::error!(
                                    "[SQLITE] rollback {table} batch process for delete error: {e}"
                                );
                            }
                            return Err(e.into());
                        }
                    };
                }
                // delete files by ids
                if !ids.is_empty() {
                    let sql = format!(
                        "UPDATE file_list SET deleted = true WHERE id IN({});",
                        ids.join(",")
                    );
                    if let Err(e) = sqlx::query(sql.as_str()).execute(&mut *tx).await {
                        if let Err(e) = tx.rollback().await {
                            log::error!(
                                "[SQLITE] rollback {table} batch process for delete error: {e}"
                            );
                        }
                        return Err(e.into());
                    }
                }
            }
        }

        if let Err(e) = tx.commit().await {
            log::error!("[SQLITE] commit {table} batch process error: {e}");
            return Err(e.into());
        }

        // release lock
        drop(client);

        Ok(())
    }
}

pub async fn create_table() -> Result<()> {
    let client = CLIENT_RW.clone();
    let client = client.lock().await;
    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS file_list
(
    id        INTEGER not null primary key autoincrement,
    account   VARCHAR not null,
    org       VARCHAR not null,
    stream    VARCHAR not null,
    date      VARCHAR not null,
    file      VARCHAR not null,
    deleted   BOOLEAN default false not null,
    flattened BOOLEAN default false not null,
    min_ts    BIGINT not null,
    max_ts    BIGINT not null,
    records   BIGINT not null,
    original_size   BIGINT not null,
    compressed_size BIGINT not null,
    index_size      BIGINT not null
);
        "#,
    )
    .execute(&*client)
    .await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS file_list_history
(
    id        INTEGER not null primary key autoincrement,
    account   VARCHAR not null,
    org       VARCHAR not null,
    stream    VARCHAR not null,
    date      VARCHAR not null,
    file      VARCHAR not null,
    deleted   BOOLEAN default false not null,
    flattened BOOLEAN default false not null,
    min_ts    BIGINT not null,
    max_ts    BIGINT not null,
    records   BIGINT not null,
    original_size   BIGINT not null,
    compressed_size BIGINT not null,
    index_size      BIGINT not null
);
        "#,
    )
    .execute(&*client)
    .await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS file_list_deleted
(
    id         INTEGER not null primary key autoincrement,
    account    VARCHAR not null,
    org        VARCHAR not null,
    stream     VARCHAR not null,
    date       VARCHAR not null,
    file       VARCHAR not null,
    index_file BOOLEAN default false not null,
    flattened  BOOLEAN default false not null,
    created_at BIGINT not null
);
        "#,
    )
    .execute(&*client)
    .await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS file_list_jobs
(
    id         INTEGER not null primary key autoincrement,
    org        VARCHAR not null,
    stream     VARCHAR not null,
    offsets    BIGINT not null,
    status     INT not null,
    node       VARCHAR not null,
    started_at BIGINT not null,
    updated_at BIGINT not null
);
        "#,
    )
    .execute(&*client)
    .await?;

    sqlx::query(
        r#"
CREATE TABLE IF NOT EXISTS stream_stats
(
    id      INTEGER not null primary key autoincrement,
    org     VARCHAR not null,
    stream  VARCHAR not null,
    file_num BIGINT not null,
    min_ts   BIGINT not null,
    max_ts   BIGINT not null,
    records  BIGINT not null,
    original_size   BIGINT not null,
    compressed_size BIGINT not null,
    index_size      BIGINT not null
);
        "#,
    )
    .execute(&*client)
    .await?;

    // create column flattened for old version <= 0.10.5
    let column = "flattened";
    let data_type = "BOOLEAN default false not null";
    add_column(&client, "file_list", column, data_type).await?;
    add_column(&client, "file_list_history", column, data_type).await?;
    add_column(&client, "file_list_deleted", column, data_type).await?;

    // create column started_at for old version <= 0.10.8
    let column = "started_at";
    let data_type = "BIGINT default 0 not null";
    add_column(&client, "file_list_jobs", column, data_type).await?;

    // create column index_size for old version <= 0.13.1
    let column = "index_size";
    let data_type = "BIGINT default 0 not null";
    add_column(&client, "file_list", column, data_type).await?;
    add_column(&client, "file_list_history", column, data_type).await?;
    add_column(&client, "stream_stats", column, data_type).await?;
    let column = "index_file";
    let data_type = "BOOLEAN default false not null";
    add_column(&client, "file_list_deleted", column, data_type).await?;

    // create col dumped for file_list_jobs for version <=0.14.0
    add_column(
        &client,
        "file_list_jobs",
        "dumped",
        "BOOLEAN default false not null",
    )
    .await?;

    // create col account for multiple object storage account support, version >= 0.14.6
    add_column(
        &client,
        "file_list",
        "account",
        "VARCHAR(32) default '' not null",
    )
    .await?;
    add_column(
        &client,
        "file_list_history",
        "account",
        "VARCHAR(32) default '' not null",
    )
    .await?;
    add_column(
        &client,
        "file_list_deleted",
        "account",
        "VARCHAR(32) default '' not null",
    )
    .await?;

    Ok(())
}

pub async fn create_table_index() -> Result<()> {
    let indices: Vec<(&str, &str, &[&str])> = vec![
        ("file_list_org_idx", "file_list", &["org"]),
        (
            "file_list_stream_ts_idx",
            "file_list",
            &["stream", "max_ts", "min_ts"],
        ),
        (
            "file_list_stream_date_idx",
            "file_list",
            &["stream", "date"],
        ),
        (
            "file_list_org_deleted_stream_idx",
            "file_list",
            &["org", "deleted", "stream"],
        ),
        ("file_list_history_org_idx", "file_list_history", &["org"]),
        (
            "file_list_history_stream_ts_idx",
            "file_list_history",
            &["stream", "max_ts", "min_ts"],
        ),
        (
            "file_list_deleted_created_at_idx",
            "file_list_deleted",
            &["org", "created_at"],
        ),
        (
            "file_list_deleted_stream_date_file_idx",
            "file_list_deleted",
            &["stream", "date", "file"],
        ),
        (
            "file_list_jobs_stream_status_idx",
            "file_list_jobs",
            &["status", "stream"],
        ),
        ("stream_stats_org_idx", "stream_stats", &["org"]),
    ];
    for (idx, table, fields) in indices {
        create_index(IndexStatement::new(idx, table, false, fields)).await?;
    }

    let unique_indices: Vec<(&str, &str, &[&str])> = vec![
        (
            "file_list_history_stream_file_idx",
            "file_list_history",
            &["stream", "date", "file"],
        ),
        (
            "file_list_jobs_stream_offsets_idx",
            "file_list_jobs",
            &["stream", "offsets"],
        ),
        ("stream_stats_stream_idx", "stream_stats", &["stream"]),
    ];
    for (idx, table, fields) in unique_indices {
        create_index(IndexStatement::new(idx, table, true, fields)).await?;
    }

    // This is a case where we want to MAKE the index unique

    let res = create_index(IndexStatement::new(
        "file_list_stream_file_idx",
        "file_list",
        true,
        &["stream", "date", "file"],
    ))
    .await;
    if let Err(e) = res {
        if !e.to_string().contains("UNIQUE constraint failed") {
            return Err(e);
        }
        // delete duplicate records
        log::warn!("[SQLITE] starting delete duplicate records");
        let client = CLIENT_RW.clone();
        let client = client.lock().await;
        let ret = sqlx::query(
                r#"SELECT stream, date, file, min(id) as id FROM file_list GROUP BY stream, date, file HAVING COUNT(*) > 1;"#,
            ).fetch_all(&*client).await?;
        log::warn!("[SQLITE] total: {} duplicate records", ret.len());
        for (i, r) in ret.iter().enumerate() {
            let stream = r.get::<String, &str>("stream");
            let date = r.get::<String, &str>("date");
            let file = r.get::<String, &str>("file");
            let id = r.get::<i64, &str>("id");
            sqlx::query(
                    r#"DELETE FROM file_list WHERE id != $1 AND stream = $2 AND date = $3 AND file = $4;"#,
                ).bind(id).bind(stream).bind(date).bind(file).execute(&*client).await?;
            if i % 1000 == 0 {
                log::warn!("[SQLITE] delete duplicate records: {}/{}", i, ret.len());
            }
        }
        drop(client);
        log::warn!(
            "[SQLITE] delete duplicate records: {}/{}",
            ret.len(),
            ret.len()
        );
        // create index again
        create_index(IndexStatement::new(
            "file_list_stream_file_idx",
            "file_list",
            true,
            &["stream", "date", "file"],
        ))
        .await?;
        log::warn!("[SQLITE] create table index(file_list_stream_file_idx) successfully");
    }

    // delete trigger for old version
    // compatible for old version <= 0.6.4
    let client = CLIENT_RW.clone();
    let client = client.lock().await;
    sqlx::query(r#"DROP TRIGGER IF EXISTS update_stream_stats_delete;"#)
        .execute(&*client)
        .await?;

    Ok(())
}

async fn add_column(
    client: &Pool<Sqlite>,
    table: &str,
    column: &str,
    data_type: &str,
) -> Result<()> {
    // Attempt to add the column, ignoring the error if the column already exists
    let check_sql = format!("ALTER TABLE {table} ADD COLUMN {column} {data_type};");
    if let Err(e) = sqlx::query(&check_sql).execute(client).await {
        // Check if the error is about the duplicate column
        if !e.to_string().contains("duplicate column name") {
            // If the error is not about the duplicate column, return it
            return Err(e.into());
        }
    }
    Ok(())
}
