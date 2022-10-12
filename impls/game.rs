use openbrush::traits::Storage;

pub use crate::traits::game::Game;
use crate::traits::participant_management::ParticipantManagement;
use crate::traits::rafle::Rafle;
use crate::traits::reward::reward::{PendindReward, Reward, RewardError};

pub const STORAGE_KEY: u32 = openbrush::storage_unique_key!(Data);

#[derive(Default, Debug )]
#[openbrush::upgradeable_storage(STORAGE_KEY)]
pub struct Data {
    _reserved: Option<()>,
}

impl<T: Storage<Data> + ParticipantManagement + Rafle + Reward> Game for T {

    default fn _play(&mut self, era: u128) -> Result<PendindReward, RewardError> {
        let participants = self._list_participants(era);
        let winners = self._run(era, participants);
        self._add_winners(era, &winners)
    }

}

