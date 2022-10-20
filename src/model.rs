use geoutils::{Distance, Location};
use jarvis_lib::config_client::SetDefaults;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub refresh_token: String,
    pub geofences: Vec<GeofenceConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GeofenceConfig {
    pub location: String,
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
pub struct TeslaVehicleStreamingData {
    pub latitude: f64,
    pub longitude: f64,
    pub charger_power: f64,
    pub odometer: f64,
}

impl TeslaVehicleStreamingData {
    pub fn inside_geofence(&self, geofence: &GeofenceConfig) -> bool {
        let tesla_location = Location::new(self.latitude, self.longitude);
        let geofence_location = Location::new(geofence.latitude, geofence.longitude);

        tesla_location
            .is_in_circle(
                &geofence_location,
                Distance::from_meters(geofence.geofence_radius_meters),
            )
            .unwrap_or(false)
    }

    pub fn in_geofence(&self, geofences: &[GeofenceConfig]) -> Option<GeofenceConfig> {
        for geofence in geofences {
            if self.inside_geofence(geofence) {
                return Some(geofence.clone());
            }
        }

        None
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
pub struct TeslaStreamingApiMessage {
    pub msg_type: String,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub value: String,
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

        assert_eq!(config.refresh_token, "abcd".to_string());
        assert_eq!(config.geofences.len(), 1);
        assert_eq!(config.geofences[0].location, "My Home".to_string());
        assert_eq!(config.geofences[0].latitude, 52.377956);
        assert_eq!(config.geofences[0].longitude, 4.897070);
        assert_eq!(config.geofences[0].geofence_radius_meters, 100.0);
    }
}
