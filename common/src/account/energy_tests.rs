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

    #[test]
    fn test_unfreeze_tos_comprehensive() {
        println!("Testing comprehensive unfreeze_tos functionality...");
        
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Test 1: Basic unfreeze functionality
        println!("  Test 1: Basic unfreeze functionality");
        resource.freeze_tos_for_energy(1000, FreezeDuration::Day7, topoheight);
        let unlock_topoheight = topoheight + FreezeDuration::Day7.duration_in_blocks();
        
        let energy_removed = resource.unfreeze_tos(500, unlock_topoheight).unwrap();
        assert_eq!(energy_removed, 550); // 500 * 1.1
        assert_eq!(resource.frozen_tos, 500);
        assert_eq!(resource.total_energy, 550);
        
        // Test 2: Unfreeze before unlock time
        println!("  Test 2: Unfreeze before unlock time");
        let result = resource.unfreeze_tos(200, unlock_topoheight - 1000);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Insufficient unlocked TOS to unfreeze");
        
        // Test 3: Unfreeze more than frozen
        println!("  Test 3: Unfreeze more than frozen");
        let result = resource.unfreeze_tos(1000, unlock_topoheight);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Insufficient frozen TOS");
        
        // Test 4: Multiple freeze records
        println!("  Test 4: Multiple freeze records");
        let mut resource2 = EnergyResource::new();
        resource2.freeze_tos_for_energy(100, FreezeDuration::Day3, topoheight);
        resource2.freeze_tos_for_energy(200, FreezeDuration::Day7, topoheight);
        resource2.freeze_tos_for_energy(300, FreezeDuration::Day14, topoheight);
        
        let max_unlock_topoheight = topoheight + FreezeDuration::Day14.duration_in_blocks();
        let energy_removed = resource2.unfreeze_tos(250, max_unlock_topoheight).unwrap();
        assert_eq!(energy_removed, 100 + 220); // 100*1.0 + 150*1.1 (partial from 200)
        assert_eq!(resource2.frozen_tos, 350); // 50 + 300 (remaining from 200 + 300)
        
        // Test 5: Edge cases
        println!("  Test 5: Edge cases");
        let mut resource3 = EnergyResource::new();
        
        // Unfreeze 0 TOS
        let result = resource3.unfreeze_tos(0, topoheight);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
        
        // Unfreeze from empty resource
        let result = resource3.unfreeze_tos(100, topoheight);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Insufficient frozen TOS");
        
        println!("✓ Comprehensive unfreeze_tos tests passed");
    }

    #[test]
    fn test_unfreeze_tos_energy_calculation_accuracy() {
        println!("Testing unfreeze_tos energy calculation accuracy...");
        
        let test_cases = vec![
            (100, FreezeDuration::Day3, 1.0),
            (200, FreezeDuration::Day7, 1.1),
            (300, FreezeDuration::Day14, 1.2),
        ];
        
        for (amount, duration, multiplier) in test_cases {
            let mut resource = EnergyResource::new();
            let topoheight = 1000;
            
            resource.freeze_tos_for_energy(amount, duration.clone(), topoheight);
            let unlock_topoheight = topoheight + duration.duration_in_blocks();
            
            // Unfreeze half the amount
            let unfreeze_amount = amount / 2;
            let energy_removed = resource.unfreeze_tos(unfreeze_amount, unlock_topoheight).unwrap();
            let expected_energy_removed = (unfreeze_amount as f64 * multiplier) as u64;
            
            assert_eq!(energy_removed, expected_energy_removed);
            println!("  ✓ {} TOS for {} days: expected {} energy, got {} energy", 
                     amount, duration.duration_in_blocks() / (24 * 60 * 60), expected_energy_removed, energy_removed);
        }
        
        println!("✓ Energy calculation accuracy tests passed");
    }

    #[test]
    fn test_unfreeze_tos_record_management() {
        println!("Testing unfreeze_tos record management...");
        
        let mut resource = EnergyResource::new();
        let topoheight = 1000;
        
        // Create multiple records with different unlock times
        resource.freeze_tos_for_energy(100, FreezeDuration::Day3, topoheight);
        resource.freeze_tos_for_energy(200, FreezeDuration::Day7, topoheight);
        resource.freeze_tos_for_energy(300, FreezeDuration::Day14, topoheight);
        
        assert_eq!(resource.freeze_records.len(), 3);
        
        // Unfreeze from earliest unlockable records first
        let unlock_3d = topoheight + FreezeDuration::Day3.duration_in_blocks();
        let energy_removed = resource.unfreeze_tos(150, unlock_3d).unwrap();
        assert_eq!(energy_removed, 100); // Only 100 from 3-day record
        assert_eq!(resource.frozen_tos, 500); // 200 + 300 remaining
        
        // Unfreeze from 7-day record
        let unlock_7d = topoheight + FreezeDuration::Day7.duration_in_blocks();
        let energy_removed2 = resource.unfreeze_tos(100, unlock_7d).unwrap();
        assert_eq!(energy_removed2, 110); // 100 * 1.1 from 7-day record
        assert_eq!(resource.frozen_tos, 400); // 100 + 300 remaining
        
        println!("✓ Record management tests passed");
    }
} 