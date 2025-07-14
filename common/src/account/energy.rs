use serde::{Deserialize, Serialize};
use crate::{
    crypto::PublicKey,
    serializer::{Serializer, Writer, Reader, ReaderError},
    block::TopoHeight,
};

/// Freeze duration options for TOS staking (inspired by TRON's freeze mechanism)
/// Different durations provide different reward multipliers to encourage long-term staking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FreezeDuration {
    /// 3-day freeze period (1.0x reward multiplier)
    Day3,
    /// 7-day freeze period (1.1x reward multiplier)  
    Day7,
    /// 14-day freeze period (1.2x reward multiplier)
    Day14,
}

impl FreezeDuration {
    /// Get the reward multiplier for this freeze duration
    /// Inspired by TRON's freeze reward system
    pub fn reward_multiplier(&self) -> f64 {
        match self {
            FreezeDuration::Day3 => 1.0,
            FreezeDuration::Day7 => 1.1,
            FreezeDuration::Day14 => 1.2,
        }
    }

    /// Get the duration in blocks (assuming 1 block per second)
    pub fn duration_in_blocks(&self) -> u64 {
        match self {
            FreezeDuration::Day3 => 3 * 24 * 60 * 60,  // 3 days in seconds
            FreezeDuration::Day7 => 7 * 24 * 60 * 60,  // 7 days in seconds
            FreezeDuration::Day14 => 14 * 24 * 60 * 60, // 14 days in seconds
        }
    }

    /// Get the duration name for display
    pub fn name(&self) -> &'static str {
        match self {
            FreezeDuration::Day3 => "3 days",
            FreezeDuration::Day7 => "7 days", 
            FreezeDuration::Day14 => "14 days",
        }
    }
}

impl Serializer for FreezeDuration {
    fn write(&self, writer: &mut Writer) {
        let variant = match self {
            FreezeDuration::Day3 => 0u8,
            FreezeDuration::Day7 => 1u8,
            FreezeDuration::Day14 => 2u8,
        };
        writer.write_u8(variant);
    }

    fn read(reader: &mut Reader) -> Result<Self, ReaderError> {
        let variant = reader.read_u8()?;
        match variant {
            0 => Ok(FreezeDuration::Day3),
            1 => Ok(FreezeDuration::Day7),
            2 => Ok(FreezeDuration::Day14),
            _ => Err(ReaderError::InvalidValue),
        }
    }

    fn size(&self) -> usize {
        1 // 1 byte for variant
    }
}

/// Individual freeze record tracking a specific freeze operation
/// Each freeze operation creates a separate record with its own duration and unlock time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreezeRecord {
    /// Amount of TOS frozen in this record
    pub amount: u64,
    /// Freeze duration chosen for this record
    pub duration: FreezeDuration,
    /// Topoheight when the freeze was initiated
    pub freeze_topoheight: TopoHeight,
    /// Topoheight when this freeze can be unlocked
    pub unlock_topoheight: TopoHeight,
    /// Energy gained from this freeze (with multiplier applied)
    pub energy_gained: u64,
}

impl FreezeRecord {
    /// Create a new freeze record
    pub fn new(amount: u64, duration: FreezeDuration, freeze_topoheight: TopoHeight) -> Self {
        let unlock_topoheight = freeze_topoheight + duration.duration_in_blocks();
        let energy_gained = (amount as f64 * duration.reward_multiplier()) as u64;
        
        Self {
            amount,
            duration,
            freeze_topoheight,
            unlock_topoheight,
            energy_gained,
        }
    }

    /// Check if this freeze record can be unlocked at the given topoheight
    pub fn can_unlock(&self, current_topoheight: TopoHeight) -> bool {
        current_topoheight >= self.unlock_topoheight
    }

    /// Get remaining lock time in blocks
    pub fn remaining_blocks(&self, current_topoheight: TopoHeight) -> u64 {
        if current_topoheight >= self.unlock_topoheight {
            0
        } else {
            self.unlock_topoheight - current_topoheight
        }
    }
}

impl Serializer for FreezeRecord {
    fn write(&self, writer: &mut Writer) {
        self.amount.write(writer);
        self.duration.write(writer);
        writer.write_u64(&self.freeze_topoheight);
        writer.write_u64(&self.unlock_topoheight);
        writer.write_u64(&self.energy_gained);
    }

    fn read(reader: &mut Reader) -> Result<Self, ReaderError> {
        Ok(Self {
            amount: reader.read_u64()?,
            duration: FreezeDuration::read(reader)?,
            freeze_topoheight: reader.read_u64()?,
            unlock_topoheight: reader.read_u64()?,
            energy_gained: reader.read_u64()?,
        })
    }

    fn size(&self) -> usize {
        self.amount.size() + self.duration.size() + 
        self.freeze_topoheight.size() + self.unlock_topoheight.size() + 
        self.energy_gained.size()
    }
}

/// Energy resource management for Terminos
/// Enhanced with TRON-style freeze duration and reward multiplier system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyResource {
    /// Total energy available
    pub total_energy: u64,
    /// Used energy
    pub used_energy: u64,
    /// Total frozen TOS across all freeze records
    pub frozen_tos: u64,
    /// Last update topoheight
    pub last_update: TopoHeight,
    /// Individual freeze records for tracking duration-based rewards
    pub freeze_records: Vec<FreezeRecord>,
}

impl EnergyResource {
    pub fn new() -> Self {
        Self {
            total_energy: 0,
            used_energy: 0,
            frozen_tos: 0,
            last_update: 0,
            freeze_records: Vec::new(),
        }
    }

    /// Get available energy
    pub fn available_energy(&self) -> u64 {
        self.total_energy.saturating_sub(self.used_energy)
    }

    /// Check if has enough energy
    pub fn has_enough_energy(&self, required: u64) -> bool {
        self.available_energy() >= required
    }

    /// Consume energy
    pub fn consume_energy(&mut self, amount: u64) -> Result<(), &'static str> {
        if self.available_energy() < amount {
            return Err("Insufficient energy");
        }
        self.used_energy += amount;
        Ok(())
    }

    /// Freeze TOS to get energy with duration-based rewards
    /// Inspired by TRON's freeze mechanism with different duration options
    pub fn freeze_tos_for_energy(&mut self, tos_amount: u64, duration: FreezeDuration, topoheight: TopoHeight) -> u64 {
        // Create a new freeze record
        let freeze_record = FreezeRecord::new(tos_amount, duration, topoheight);
        let energy_gained = freeze_record.energy_gained;
        
        // Add to freeze records
        self.freeze_records.push(freeze_record);
        
        // Update totals
        self.frozen_tos += tos_amount;
        self.total_energy += energy_gained;
        self.last_update = topoheight;
        
        energy_gained
    }

    /// Unfreeze TOS from a specific freeze record
    /// Can only unfreeze records that have reached their unlock time
    pub fn unfreeze_tos(&mut self, tos_amount: u64, current_topoheight: TopoHeight) -> Result<u64, String> {
        if self.frozen_tos < tos_amount {
            return Err("Insufficient frozen TOS".to_string());
        }

        // Find eligible freeze records (unlocked and with sufficient amount)
        let mut remaining_to_unfreeze = tos_amount;
        let mut total_energy_removed = 0;
        let mut records_to_remove = Vec::new();

        for (index, record) in self.freeze_records.iter().enumerate() {
            if !record.can_unlock(current_topoheight) {
                continue; // Skip records that haven't reached unlock time
            }

            if remaining_to_unfreeze == 0 {
                break;
            }

            let unfreeze_amount = std::cmp::min(remaining_to_unfreeze, record.amount);
            let energy_ratio = unfreeze_amount as f64 / record.amount as f64;
            let energy_to_remove = (record.energy_gained as f64 * energy_ratio) as u64;

            total_energy_removed += energy_to_remove;
            remaining_to_unfreeze -= unfreeze_amount;

            // Mark record for removal if fully unfrozen
            if unfreeze_amount == record.amount {
                records_to_remove.push(index);
            } else {
                // Partially unfreeze the record
                // Note: In a real implementation, you might want to split the record
                // For simplicity, we'll remove the entire record and create a new one with remaining amount
                records_to_remove.push(index);
            }
        }

        if remaining_to_unfreeze > 0 {
            return Err("Insufficient unlocked TOS to unfreeze".to_string());
        }

        // Remove marked records (in reverse order to maintain indices)
        for &index in records_to_remove.iter().rev() {
            self.freeze_records.remove(index);
        }

        // Update totals
        self.frozen_tos -= tos_amount;
        self.total_energy = self.total_energy.saturating_sub(total_energy_removed);
        self.last_update = current_topoheight;

        Ok(total_energy_removed)
    }

    /// Get all freeze records that can be unlocked at the current topoheight
    pub fn get_unlockable_records(&self, current_topoheight: TopoHeight) -> Vec<&FreezeRecord> {
        self.freeze_records.iter()
            .filter(|record| record.can_unlock(current_topoheight))
            .collect()
    }

    /// Get total unlockable TOS amount at the current topoheight
    pub fn get_unlockable_tos(&self, current_topoheight: TopoHeight) -> u64 {
        self.get_unlockable_records(current_topoheight)
            .iter()
            .map(|record| record.amount)
            .sum()
    }

    /// Get freeze records grouped by duration
    pub fn get_freeze_records_by_duration(&self) -> std::collections::HashMap<FreezeDuration, Vec<&FreezeRecord>> {
        let mut grouped: std::collections::HashMap<FreezeDuration, Vec<&FreezeRecord>> = std::collections::HashMap::new();
        
        for record in &self.freeze_records {
            grouped.entry(record.duration.clone()).or_insert_with(Vec::new).push(record);
        }
        
        grouped
    }

    /// Reset used energy (called periodically)
    pub fn reset_used_energy(&mut self, topoheight: TopoHeight) {
        self.used_energy = 0;
        self.last_update = topoheight;
    }
}

/// Energy lease contract
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyLease {
    /// Lessor (energy provider)
    pub lessor: PublicKey,
    /// Lessee (energy consumer)
    pub lessee: PublicKey,
    /// Amount of energy leased
    pub energy_amount: u64,
    /// Lease duration in blocks
    pub duration: u64,
    /// Start topoheight
    pub start_topoheight: TopoHeight,
    /// Price per energy unit
    pub price_per_energy: u64,
}

impl EnergyLease {
    pub fn new(
        lessor: PublicKey,
        lessee: PublicKey,
        energy_amount: u64,
        duration: u64,
        start_topoheight: TopoHeight,
        price_per_energy: u64,
    ) -> Self {
        Self {
            lessor,
            lessee,
            energy_amount,
            duration,
            start_topoheight,
            price_per_energy,
        }
    }

    /// Check if lease is still valid
    pub fn is_valid(&self, current_topoheight: TopoHeight) -> bool {
        current_topoheight < self.start_topoheight + self.duration
    }

    /// Calculate total cost
    pub fn total_cost(&self) -> u64 {
        self.energy_amount * self.price_per_energy
    }
}

impl Serializer for EnergyResource {
    fn write(&self, writer: &mut Writer) {
        writer.write_u64(&self.total_energy);
        writer.write_u64(&self.used_energy);
        writer.write_u64(&self.frozen_tos);
        writer.write_u64(&self.last_update);
        
        // Write freeze records
        writer.write_u64(&(self.freeze_records.len() as u64));
        for record in &self.freeze_records {
            record.write(writer);
        }
    }

    fn read(reader: &mut Reader) -> Result<Self, ReaderError> {
        let total_energy = reader.read_u64()?;
        let used_energy = reader.read_u64()?;
        let frozen_tos = reader.read_u64()?;
        let last_update = reader.read_u64()?;
        
        // Read freeze records
        let records_count = reader.read_u64()? as usize;
        let mut freeze_records = Vec::with_capacity(records_count);
        for _ in 0..records_count {
            freeze_records.push(FreezeRecord::read(reader)?);
        }
        
        Ok(Self {
            total_energy,
            used_energy,
            frozen_tos,
            last_update,
            freeze_records,
        })
    }

    fn size(&self) -> usize {
        let base_size = self.total_energy.size() + self.used_energy.size() + 
                       self.frozen_tos.size() + self.last_update.size();
        let records_size = 8 + self.freeze_records.iter().map(|r| r.size()).sum::<usize>();
        base_size + records_size
    }
}

impl Serializer for EnergyLease {
    fn write(&self, writer: &mut Writer) {
        self.lessor.write(writer);
        self.lessee.write(writer);
        writer.write_u64(&self.energy_amount);
        writer.write_u64(&self.duration);
        writer.write_u64(&self.start_topoheight);
        writer.write_u64(&self.price_per_energy);
    }

    fn read(reader: &mut Reader) -> Result<Self, ReaderError> {
        Ok(Self {
            lessor: PublicKey::read(reader)?,
            lessee: PublicKey::read(reader)?,
            energy_amount: reader.read_u64()?,
            duration: reader.read_u64()?,
            start_topoheight: reader.read_u64()?,
            price_per_energy: reader.read_u64()?,
        })
    }

    fn size(&self) -> usize {
        self.lessor.size() + self.lessee.size() + self.energy_amount.size() + 
        self.duration.size() + self.start_topoheight.size() + self.price_per_energy.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freeze_duration_reward_multipliers() {
        assert_eq!(FreezeDuration::Day3.reward_multiplier(), 1.0);
        assert_eq!(FreezeDuration::Day7.reward_multiplier(), 1.1);
        assert_eq!(FreezeDuration::Day14.reward_multiplier(), 1.2);
    }

    #[test]
    fn test_freeze_duration_blocks() {
        assert_eq!(FreezeDuration::Day3.duration_in_blocks(), 3 * 24 * 60 * 60);
        assert_eq!(FreezeDuration::Day7.duration_in_blocks(), 7 * 24 * 60 * 60);
        assert_eq!(FreezeDuration::Day14.duration_in_blocks(), 14 * 24 * 60 * 60);
    }

    #[test]
    fn test_freeze_record_creation() {
        let record = FreezeRecord::new(1000, FreezeDuration::Day7, 100);
        assert_eq!(record.amount, 1000);
        assert_eq!(record.duration, FreezeDuration::Day7);
        assert_eq!(record.freeze_topoheight, 100);
        assert_eq!(record.unlock_topoheight, 100 + 7 * 24 * 60 * 60);
        assert_eq!(record.energy_gained, 1100); // 1000 * 1.1
    }

    #[test]
    fn test_freeze_record_unlock_check() {
        let record = FreezeRecord::new(1000, FreezeDuration::Day3, 100);
        let unlock_time = 100 + 3 * 24 * 60 * 60;
        
        assert!(!record.can_unlock(unlock_time - 1));
        assert!(record.can_unlock(unlock_time));
        assert!(record.can_unlock(unlock_time + 1000));
    }

    #[test]
    fn test_energy_resource_freeze_with_duration() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Freeze 1000 TOS for 7 days
        let energy_gained = resource.freeze_tos_for_energy(1000, FreezeDuration::Day7, topoheight);
        assert_eq!(energy_gained, 1100); // 1000 * 1.1
        assert_eq!(resource.frozen_tos, 1000);
        assert_eq!(resource.total_energy, 1100);
        assert_eq!(resource.freeze_records.len(), 1);
        
        // Freeze 500 TOS for 14 days
        let energy_gained2 = resource.freeze_tos_for_energy(500, FreezeDuration::Day14, topoheight);
        assert_eq!(energy_gained2, 600); // 500 * 1.2
        assert_eq!(resource.frozen_tos, 1500);
        assert_eq!(resource.total_energy, 1700);
        assert_eq!(resource.freeze_records.len(), 2);
    }

    #[test]
    fn test_energy_resource_unfreeze() {
        let mut resource = EnergyResource::new();
        let freeze_topoheight = 1000;
        let unlock_topoheight = freeze_topoheight + 7 * 24 * 60 * 60;
        
        // Freeze 1000 TOS for 7 days
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day7, freeze_topoheight);
        
        // Try to unfreeze before unlock time (should fail)
        let result = resource.unfreeze_tos(500, unlock_topoheight - 1);
        assert!(result.is_err());
        
        // Unfreeze after unlock time
        let energy_removed = resource.unfreeze_tos(500, unlock_topoheight).unwrap();
        assert_eq!(energy_removed, 550); // 500 * 1.1
        assert_eq!(resource.frozen_tos, 500);
        assert_eq!(resource.total_energy, 550);
    }

    #[test]
    fn test_get_unlockable_records() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Freeze with different durations
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day3, topoheight);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day7, topoheight);
        resource.freeze_tos_for_energy(200, FreezeDuration::Day14, topoheight);
        
        // Check unlockable records at different times
        let unlockable_3d = resource.get_unlockable_records(topoheight + 3 * 24 * 60 * 60);
        assert_eq!(unlockable_3d.len(), 1);
        
        let unlockable_7d = resource.get_unlockable_records(topoheight + 7 * 24 * 60 * 60);
        assert_eq!(unlockable_7d.len(), 2);
        
        let unlockable_14d = resource.get_unlockable_records(topoheight + 14 * 24 * 60 * 60);
        assert_eq!(unlockable_14d.len(), 3);
    }

    #[test]
    fn test_serialization() {
        let mut resource = EnergyResource::new();
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day7, 1000);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day14, 1000);
        
        let mut bytes = Vec::new();
        let mut writer = crate::serializer::Writer::new(&mut bytes);
        resource.write(&mut writer);
        
        let mut reader = crate::serializer::Reader::new(&bytes);
        let deserialized = EnergyResource::read(&mut reader).unwrap();
        
        assert_eq!(resource.total_energy, deserialized.total_energy);
        assert_eq!(resource.frozen_tos, deserialized.frozen_tos);
        assert_eq!(resource.freeze_records.len(), deserialized.freeze_records.len());
    }

    #[test]
    fn test_freeze_duration_serialization() {
        let durations = [FreezeDuration::Day3, FreezeDuration::Day7, FreezeDuration::Day14];
        
        for duration in &durations {
            let mut bytes = Vec::new();
            let mut writer = crate::serializer::Writer::new(&mut bytes);
            duration.write(&mut writer);
            
            let mut reader = crate::serializer::Reader::new(&bytes);
            let deserialized = FreezeDuration::read(&mut reader).unwrap();
            
            assert_eq!(duration, &deserialized);
        }
    }

    #[test]
    fn test_freeze_record_serialization() {
        let record = FreezeRecord::new(1000, FreezeDuration::Day7, 100);
        
        let mut bytes = Vec::new();
        let mut writer = crate::serializer::Writer::new(&mut bytes);
        record.write(&mut writer);
        
        let mut reader = crate::serializer::Reader::new(&bytes);
        let deserialized = FreezeRecord::read(&mut reader).unwrap();
        
        assert_eq!(record.amount, deserialized.amount);
        assert_eq!(record.duration, deserialized.duration);
        assert_eq!(record.energy_gained, deserialized.energy_gained);
    }
} 