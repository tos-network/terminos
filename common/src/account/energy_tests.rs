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
    fn test_freeze_duration_names() {
        assert_eq!(FreezeDuration::Day3.name(), "3 days");
        assert_eq!(FreezeDuration::Day7.name(), "7 days");
        assert_eq!(FreezeDuration::Day14.name(), "14 days");
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
    fn test_freeze_record_remaining_blocks() {
        let record = FreezeRecord::new(1000, FreezeDuration::Day3, 100);
        let unlock_time = 100 + 3 * 24 * 60 * 60;
        
        assert_eq!(record.remaining_blocks(unlock_time - 1000), 1000);
        assert_eq!(record.remaining_blocks(unlock_time), 0);
        assert_eq!(record.remaining_blocks(unlock_time + 1000), 0);
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
    fn test_get_unlockable_tos() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Freeze with different durations
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day3, topoheight);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day7, topoheight);
        
        // Check unlockable TOS at different times
        let unlockable_3d = resource.get_unlockable_tos(topoheight + 3 * 24 * 60 * 60);
        assert_eq!(unlockable_3d, 1000);
        
        let unlockable_7d = resource.get_unlockable_tos(topoheight + 7 * 24 * 60 * 60);
        assert_eq!(unlockable_7d, 1500);
    }

    #[test]
    fn test_get_freeze_records_by_duration() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Freeze with different durations
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day3, topoheight);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day7, topoheight);
        resource.freeze_tos_for_energy(200, FreezeDuration::Day14, topoheight);
        resource.freeze_tos_for_energy(300, FreezeDuration::Day7, topoheight); // Another 7-day freeze
        
        let grouped = resource.get_freeze_records_by_duration();
        
        assert_eq!(grouped.get(&FreezeDuration::Day3).unwrap().len(), 1);
        assert_eq!(grouped.get(&FreezeDuration::Day7).unwrap().len(), 2);
        assert_eq!(grouped.get(&FreezeDuration::Day14).unwrap().len(), 1);
    }

    #[test]
    fn test_serialization() {
        let mut resource = EnergyResource::new();
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day7, 1000);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day14, 1000);
        
        let mut writer = crate::serializer::Writer::default();
        resource.write(&mut writer);
        
        let mut reader = crate::serializer::Reader::new(&writer.bytes());
        let deserialized = EnergyResource::read(&mut reader).unwrap();
        
        assert_eq!(resource.total_energy, deserialized.total_energy);
        assert_eq!(resource.frozen_tos, deserialized.frozen_tos);
        assert_eq!(resource.freeze_records.len(), deserialized.freeze_records.len());
    }

    #[test]
    fn test_freeze_duration_serialization() {
        let durations = [FreezeDuration::Day3, FreezeDuration::Day7, FreezeDuration::Day14];
        
        for duration in durations {
            let mut writer = crate::serializer::Writer::default();
            duration.write(&mut writer);
            
            let mut reader = crate::serializer::Reader::new(&writer.bytes());
            let deserialized = FreezeDuration::read(&mut reader).unwrap();
            
            assert_eq!(duration, deserialized);
        }
    }

    #[test]
    fn test_freeze_record_serialization() {
        let record = FreezeRecord::new(1000, FreezeDuration::Day7, 100);
        
        let mut writer = crate::serializer::Writer::default();
        record.write(&mut writer);
        
        let mut reader = crate::serializer::Reader::new(&writer.bytes());
        let deserialized = FreezeRecord::read(&mut reader).unwrap();
        
        assert_eq!(record.amount, deserialized.amount);
        assert_eq!(record.duration, deserialized.duration);
        assert_eq!(record.energy_gained, deserialized.energy_gained);
    }

    #[test]
    fn test_energy_consumption_with_freeze_records() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Freeze TOS to get energy
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day7, topoheight);
        assert_eq!(resource.available_energy(), 1100);
        
        // Consume energy
        resource.consume_energy(500).unwrap();
        assert_eq!(resource.available_energy(), 600);
        assert_eq!(resource.used_energy, 500);
        
        // Reset energy usage
        resource.reset_used_energy(topoheight + 100);
        assert_eq!(resource.available_energy(), 1100);
        assert_eq!(resource.used_energy, 0);
    }

    #[test]
    fn test_multiple_freeze_operations() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Multiple freeze operations with different durations
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day3, topoheight);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day7, topoheight);
        resource.freeze_tos_for_energy(200, FreezeDuration::Day14, topoheight);
        
        // Check totals
        assert_eq!(resource.frozen_tos, 1700);
        assert_eq!(resource.total_energy, 1000 + 550 + 240); // 1000*1.0 + 500*1.1 + 200*1.2
        assert_eq!(resource.freeze_records.len(), 3);
    }

    #[test]
    fn test_partial_unfreeze_scenarios() {
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Freeze multiple amounts
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day3, topoheight);
        resource.freeze_tos_for_energy(500, FreezeDuration::Day7, topoheight);
        
        let unlock_time_3d = topoheight + 3 * 24 * 60 * 60;
        let unlock_time_7d = topoheight + 7 * 24 * 60 * 60;
        
        // Try to unfreeze more than available (should fail)
        let result = resource.unfreeze_tos(2000, unlock_time_7d);
        assert!(result.is_err());
        
        // Unfreeze exactly what's available
        let energy_removed = resource.unfreeze_tos(1500, unlock_time_7d).unwrap();
        assert_eq!(energy_removed, 1000 + 550); // 1000*1.0 + 500*1.1
        assert_eq!(resource.frozen_tos, 0);
        assert_eq!(resource.total_energy, 0);
    }
} 