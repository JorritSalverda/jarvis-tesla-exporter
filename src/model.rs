use geoutils::Location;
use jarvis_lib::config_client::SetDefaults;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub location: String,
    pub refresh_token: String,
    pub latitude: f64,
    pub longitude: f64,
    pub geofence_radius_meters: f64,
}

impl SetDefaults for Config {
    fn set_defaults(&mut self) {}
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]

pub struct TeslaAccessTokenRequest {
    pub grant_type: String,
    pub scope: String,
    pub client_id: String,
    pub refresh_token: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TeslaAccessToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: usize,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TeslaApiResponse<T> {
    pub response: T,
    pub count: usize,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case", from = "String")]
pub enum TeslaVehicleState {
    Online,
    Asleep,
    Charging,
    Driving,
    Updating,
    Other(String),
}

impl From<String> for TeslaVehicleState {
    fn from(s: String) -> Self {
        use TeslaVehicleState::*;

        return match s.as_str() {
            "online" => Online,
            "asleep" => Asleep,
            "charging" => Charging,
            "driving" => Driving,
            "updating" => Updating,
            _ => Other(s),
        };
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TeslaVehicle {
    pub id: usize,
    pub vehicle_id: usize,
    pub vin: String,
    pub display_name: String,
    pub state: TeslaVehicleState,
    pub in_service: bool,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TeslaVehicleData {
    pub id: usize,
    pub user_id: usize,
    pub vehicle_id: usize,
    pub vin: String,
    pub display_name: String,
    pub state: TeslaVehicleState,
    pub in_service: bool,

    pub charge_state: TeslaVehicleChargeState,
    pub drive_state: TeslaVehicleDriveState,
}

impl TeslaVehicleData {
    pub fn inside_geofence(&self, latitude: f64, longitude: f64, radius: f64) -> bool {
        let tesla_location = Location::new(self.drive_state.latitude, self.drive_state.longitude);
        let geofence_location = Location::new(latitude, longitude);

        if let Ok(distance) = tesla_location.distance_to(&geofence_location) {
            distance.meters() < radius
        } else {
            false
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TeslaVehicleChargeState {
    pub battery_level: f64,
    pub charge_amps: f64,
    pub charge_current_request: f64,
    pub charge_current_request_max: f64,
    pub charge_enable_request: bool,
    pub charge_energy_added: f64,
    pub charge_rate: f64,

    pub charger_actual_current: f64,
    pub charger_phases: f64,
    pub charger_power: f64,
    pub charger_voltage: f64,
    pub charging_state: String,

    pub timestamp: usize,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TeslaVehicleDriveState {
    pub gps_as_of: usize,
    pub latitude: f64,
    pub longitude: f64,
    pub heading: f64,
    pub timestamp: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_lib::config_client::{ConfigClient, ConfigClientConfig};

    #[test]
    fn read_config_from_file_returns_deserialized_test_file() {
        let config_client =
            ConfigClient::new(ConfigClientConfig::new("test-config.yaml".to_string()).unwrap());

        let config: Config = config_client.read_config_from_file().unwrap();

        assert_eq!(config.location, "My Home".to_string());
        assert_eq!(config.refresh_token, "abcd".to_string());
        assert_eq!(config.latitude, 52.377956);
        assert_eq!(config.longitude, 4.897070);
        assert_eq!(config.geofence_radius_meters, 100.0);
    }
}
