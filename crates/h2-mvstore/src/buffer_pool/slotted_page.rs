use byteorder::{BigEndian, ByteOrder};
use crc32fast::Hasher;

pub const PAGE_SIZE: usize = 8192;
pub const PAGE_HEADER_SIZE: usize = 32;
pub const SLOT_SIZE: usize = 4; // offset (2 bytes) + length (2 bytes)

/// 8KB バイナリ Slotted Page
///
/// レイアウト:
/// [0..4]   page_id (u32)
/// [4..12]  lsn (u64)
/// [12]     page_type (u8, 0: Data, 1: Index, 2: Free)
/// [13]     flags (u8)
/// [14..16] tuple_count (u16)
/// [16..18] free_space_lower (u16, スロット配列の終端オフセット)
/// [18..20] free_space_upper (u16, タプルデータの開始オフセット)
/// [20..24] checksum (u32)
/// [24..32] 予約領域 (8 bytes)
/// [32..lower] Slot 配列 (各 4 bytes: offset: u16, length: u16)
/// [lower..upper] 空き領域
/// [upper..8192] タプル実体データ (後方から前方に向かって成長)
pub struct SlottedPage;

impl SlottedPage {
    /// 新規ページを初期化
    pub fn init(buf: &mut [u8; PAGE_SIZE], page_id: u32, page_type: u8) {
        buf.fill(0);
        BigEndian::write_u32(&mut buf[0..4], page_id);
        BigEndian::write_u64(&mut buf[4..12], 0); // lsn
        buf[12] = page_type;
        buf[13] = 0; // flags
        BigEndian::write_u16(&mut buf[14..16], 0); // tuple_count
        BigEndian::write_u16(&mut buf[16..18], PAGE_HEADER_SIZE as u16); // free_space_lower
        BigEndian::write_u16(&mut buf[18..20], PAGE_SIZE as u16); // free_space_upper
        BigEndian::write_u32(&mut buf[20..24], 0); // checksum placeholder
    }

    pub fn get_page_id(buf: &[u8; PAGE_SIZE]) -> u32 {
        BigEndian::read_u32(&buf[0..4])
    }

    pub fn set_page_id(buf: &mut [u8; PAGE_SIZE], page_id: u32) {
        BigEndian::write_u32(&mut buf[0..4], page_id);
    }

    pub fn get_lsn(buf: &[u8; PAGE_SIZE]) -> u64 {
        BigEndian::read_u64(&buf[4..12])
    }

    pub fn set_lsn(buf: &mut [u8; PAGE_SIZE], lsn: u64) {
        BigEndian::write_u64(&mut buf[4..12], lsn);
    }

    pub fn get_page_type(buf: &[u8; PAGE_SIZE]) -> u8 {
        buf[12]
    }

    pub fn get_tuple_count(buf: &[u8; PAGE_SIZE]) -> u16 {
        BigEndian::read_u16(&buf[14..16])
    }

    pub fn get_free_space_lower(buf: &[u8; PAGE_SIZE]) -> usize {
        BigEndian::read_u16(&buf[16..18]) as usize
    }

    pub fn get_free_space_upper(buf: &[u8; PAGE_SIZE]) -> usize {
        BigEndian::read_u16(&buf[18..20]) as usize
    }

    /// 空き容量（バイト数）を取得
    pub fn get_free_space(buf: &[u8; PAGE_SIZE]) -> usize {
        let lower = Self::get_free_space_lower(buf);
        let upper = Self::get_free_space_upper(buf);
        if upper >= lower {
            upper - lower
        } else {
            0
        }
    }

    /// タプルを挿入。成功した場合は slot_id (0-indexed) を返す。
    pub fn insert_tuple(buf: &mut [u8; PAGE_SIZE], data: &[u8]) -> Option<u16> {
        let data_len = data.len();
        let needed_space = data_len + SLOT_SIZE;
        let free_space = Self::get_free_space(buf);

        if free_space < needed_space {
            return None; // 空き容量不足
        }

        let lower = Self::get_free_space_lower(buf);
        let upper = Self::get_free_space_upper(buf);

        let new_upper = upper - data_len;
        let new_lower = lower + SLOT_SIZE;

        // タプルデータをコピー
        buf[new_upper..upper].copy_from_slice(data);

        // スロット情報を書き込み
        let slot_offset = lower;
        BigEndian::write_u16(&mut buf[slot_offset..slot_offset + 2], new_upper as u16);
        BigEndian::write_u16(&mut buf[slot_offset + 2..slot_offset + 4], data_len as u16);

        // ヘッダ更新
        let tuple_count = Self::get_tuple_count(buf);
        BigEndian::write_u16(&mut buf[14..16], tuple_count + 1);
        BigEndian::write_u16(&mut buf[16..18], new_lower as u16);
        BigEndian::write_u16(&mut buf[18..20], new_upper as u16);

        Some(tuple_count)
    }

    /// slot_id に対応するタプル参照を取得
    pub fn get_tuple<'a>(buf: &'a [u8; PAGE_SIZE], slot_id: u16) -> Option<&'a [u8]> {
        let tuple_count = Self::get_tuple_count(buf);
        if slot_id >= tuple_count {
            return None;
        }

        let slot_offset = PAGE_HEADER_SIZE + (slot_id as usize) * SLOT_SIZE;
        let offset = BigEndian::read_u16(&buf[slot_offset..slot_offset + 2]) as usize;
        let len = BigEndian::read_u16(&buf[slot_offset + 2..slot_offset + 4]) as usize;

        if offset == 0 || len == 0 || offset + len > PAGE_SIZE {
            None // 削除済みまたは無効
        } else {
            Some(&buf[offset..offset + len])
        }
    }

    /// タプルを削除（論理削除、オフセットを0に設定）
    pub fn delete_tuple(buf: &mut [u8; PAGE_SIZE], slot_id: u16) -> bool {
        let tuple_count = Self::get_tuple_count(buf);
        if slot_id >= tuple_count {
            return false;
        }

        let slot_offset = PAGE_HEADER_SIZE + (slot_id as usize) * SLOT_SIZE;
        BigEndian::write_u16(&mut buf[slot_offset..slot_offset + 2], 0);
        BigEndian::write_u16(&mut buf[slot_offset + 2..slot_offset + 4], 0);
        true
    }

    /// 有効（生存）タプル数を取得
    pub fn live_tuple_count(buf: &[u8; PAGE_SIZE]) -> usize {
        let tuple_count = Self::get_tuple_count(buf);
        let mut live = 0;
        for slot_id in 0..tuple_count {
            let slot_offset = PAGE_HEADER_SIZE + (slot_id as usize) * SLOT_SIZE;
            let offset = BigEndian::read_u16(&buf[slot_offset..slot_offset + 2]) as usize;
            let len = BigEndian::read_u16(&buf[slot_offset + 2..slot_offset + 4]) as usize;
            if offset != 0 && len != 0 && offset + len <= PAGE_SIZE {
                live += 1;
            }
        }
        live
    }

    /// ページ内のすべてのタプルが削除（デッド）されているか確認
    pub fn is_all_dead(buf: &[u8; PAGE_SIZE]) -> bool {
        Self::live_tuple_count(buf) == 0
    }

    /// ページを空きページ（Free Page, page_type = 2）としてマーク・初期化
    pub fn mark_as_free(buf: &mut [u8; PAGE_SIZE]) {
        let page_id = Self::get_page_id(buf);
        Self::init(buf, page_id, 2);
    }

    /// 削除されたタプル領域を回収（デフラグメンテーション）
    pub fn defragment(buf: &mut [u8; PAGE_SIZE]) {
        let tuple_count = Self::get_tuple_count(buf);
        let mut temp_data = Vec::new();
        let mut valid_slots = Vec::new();

        for slot_id in 0..tuple_count {
            let slot_offset = PAGE_HEADER_SIZE + (slot_id as usize) * SLOT_SIZE;
            let offset = BigEndian::read_u16(&buf[slot_offset..slot_offset + 2]) as usize;
            let len = BigEndian::read_u16(&buf[slot_offset + 2..slot_offset + 4]) as usize;

            if offset != 0 && len != 0 && offset + len <= PAGE_SIZE {
                let data = buf[offset..offset + len].to_vec();
                temp_data.push(data);
                valid_slots.push(slot_id);
            }
        }

        let mut current_upper = PAGE_SIZE;
        for (i, data) in temp_data.iter().enumerate() {
            let slot_id = valid_slots[i];
            current_upper -= data.len();
            buf[current_upper..current_upper + data.len()].copy_from_slice(data);

            let slot_offset = PAGE_HEADER_SIZE + (slot_id as usize) * SLOT_SIZE;
            BigEndian::write_u16(&mut buf[slot_offset..slot_offset + 2], current_upper as u16);
            BigEndian::write_u16(&mut buf[slot_offset + 2..slot_offset + 4], data.len() as u16);
        }

        BigEndian::write_u16(&mut buf[18..20], current_upper as u16);
    }

    /// チェックサムを計算して更新
    pub fn compute_and_write_checksum(buf: &mut [u8; PAGE_SIZE]) {
        BigEndian::write_u32(&mut buf[20..24], 0);
        let mut hasher = Hasher::new();
        hasher.update(buf);
        let checksum = hasher.finalize();
        BigEndian::write_u32(&mut buf[20..24], checksum);
    }

    /// チェックサムを検証
    pub fn verify_checksum(buf: &[u8; PAGE_SIZE]) -> bool {
        let stored_checksum = BigEndian::read_u32(&buf[20..24]);
        if stored_checksum == 0 {
            return true; // 未計算またはスキップ
        }
        let mut copy = *buf;
        BigEndian::write_u32(&mut copy[20..24], 0);
        let mut hasher = Hasher::new();
        hasher.update(&copy);
        hasher.finalize() == stored_checksum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slotted_page_insert_and_get() {
        let mut page = [0u8; PAGE_SIZE];
        SlottedPage::init(&mut page, 1, 0);

        let slot0 = SlottedPage::insert_tuple(&mut page, b"hello world").unwrap();
        assert_eq!(slot0, 0);
        let slot1 = SlottedPage::insert_tuple(&mut page, b"rust database").unwrap();
        assert_eq!(slot1, 1);

        assert_eq!(SlottedPage::get_tuple(&page, 0), Some(&b"hello world"[..]));
        assert_eq!(SlottedPage::get_tuple(&page, 1), Some(&b"rust database"[..]));
        assert_eq!(SlottedPage::get_tuple(&page, 2), None);

        // 削除
        assert!(SlottedPage::delete_tuple(&mut page, 0));
        assert_eq!(SlottedPage::get_tuple(&page, 0), None);
        assert_eq!(SlottedPage::get_tuple(&page, 1), Some(&b"rust database"[..]));

        // デフラグ
        SlottedPage::defragment(&mut page);
        assert_eq!(SlottedPage::get_tuple(&page, 1), Some(&b"rust database"[..]));

        // チェックサム
        SlottedPage::compute_and_write_checksum(&mut page);
        assert!(SlottedPage::verify_checksum(&page));
    }
}
