use std::error::Error;
use std::time::Instant;

use chrono::Utc;
use jarvis_lib::model::{EntityType, MetricType, Sample, SampleType};
use jarvis_lib::{measurement_client::MeasurementClient, model::Measurement};
use log::{debug, info, warn};
use reqwest::Url;
use retry::delay::{jitter, Exponential};
use retry::retry;
use serde_json::Value;
use tungstenite::{connect, Message};
use uuid::Uuid;

use crate::model::{
    Config, TeslaAccessToken, TeslaAccessTokenRequest, TeslaApiResponse, TeslaStreamingApiMessage,
    TeslaVehicle, TeslaVehicleChargeState, TeslaVehicleState, TeslaVehicleStreamingData,
};

const RETRY_INTERVAL_MS: u64 = 100;
const RETRY_FACTOR: f64 = 2.0;
const RETRY_TAKES: usize = 3;

pub struct TeslaApiClient {}

impl MeasurementClient<Config> for TeslaApiClient {
    fn get_measurements(
        &self,
        config: Config,
        last_measurements: Option<Vec<Measurement>>,
    ) -> Result<Vec<Measurement>, Box<dyn Error>> {
        let mut measurements: Vec<Measurement> = vec![];

        let token = self.get_access_token(&config)?;

        let vehicles = self.get_vehicles(&token)?;
        if let Some(vehicle) = vehicles.into_iter().next() {
            debug!(
                "State for vehicle {}: {:?}",
                vehicle.display_name, vehicle.state
            );

            let (last_location, last_charger_power, last_charge_energy_added, last_odometer) =
                self.get_last_values(last_measurements, &vehicle);

            let (location, charger_power, charge_energy_added, odometer) = if vehicle.in_service
                || vehicle.state == TeslaVehicleState::Asleep
            {
                info!("Vehicle is asleep or in service");
                // vehicle is asleep or in service, return last values

                (last_location, 0.0, last_charge_energy_added, last_odometer)
            } else {
                info!("Vehicle is awake");
                // vehicle is online; get stream to check location and power without keeping vehicle awake
                match retry(
                    Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                        .map(jitter)
                        .take(RETRY_TAKES),
                    || self.get_streaming_data(&token, &vehicle),
                ) {
                    Ok(vehicle_data) => {
                        debug!("streaming vehicle_data: {:?}", vehicle_data);

                        let location =
                            if let Some(geofence) = vehicle_data.in_geofence(&config.geofences) {
                                info!("Vehicle is inside geofence {}", geofence.location);
                                geofence.location
                            } else {
                                info!("Vehicle is outside all geofences");
                                last_location
                            };

                        let current_charger_power = vehicle_data.charger_power * 1000.0;

                        let current_charge_energy_added =
                            if current_charger_power > 0.0 || last_charger_power > 0.0 {
                                // get vehicle data through regular api if vehicle is charging or has just finished charging
                                // skip otherwise, because it keeps the vehicle awake
                                let vehicle_charge_state =
                                    self.get_vehicle_charge_state(&token, &vehicle)?;

                                debug!("restful vehicle_charge_state: {:?}", vehicle_charge_state);

                                vehicle_charge_state.charge_energy_added * 1000.0 * 3600.0
                            } else {
                                last_charge_energy_added
                            };

                        // convert miles to meters
                        let current_odometer = vehicle_data.odometer * 1609.344;

                        (
                            location,
                            current_charger_power,
                            current_charge_energy_added,
                            current_odometer,
                        )
                    }
                    Err(e) => {
                        warn!("Stream returned error {}", e);
                        info!("Vehicle doesn't seem awake, handling like it's asleep");

                        (last_location, 0.0, last_charge_energy_added, last_odometer)
                    }
                }
            };

            let mut measurement = Measurement {
                id: Uuid::new_v4().to_string(),
                source: String::from("jarvis-tesla-exporter"),
                location,
                samples: Vec::new(),
                measured_at_time: Utc::now(),
            };

            // store as gauge for timeline graphs
            measurement.samples.push(Sample {
                entity_type: EntityType::Device,
                entity_name: "jarvis-tesla-exporter".into(),
                sample_type: SampleType::ElectricityConsumption,
                sample_name: vehicle.display_name.clone(),
                metric_type: MetricType::Gauge,
                value: charger_power,
            });

            // store as counter for totals
            measurement.samples.push(Sample {
                entity_type: EntityType::Device,
                entity_name: "jarvis-tesla-exporter".into(),
                sample_type: SampleType::ElectricityConsumption,
                sample_name: vehicle.display_name.clone(),
                metric_type: MetricType::Counter,
                value: charge_energy_added,
            });

            // odometer counter
            measurement.samples.push(Sample {
                entity_type: EntityType::Device,
                entity_name: "jarvis-tesla-exporter".into(),
                sample_type: SampleType::DistanceTraveled,
                sample_name: vehicle.display_name,
                metric_type: MetricType::Counter,
                value: odometer,
            });

            debug!("measurement: {:?}", measurement);

            measurements.push(measurement);
        }

        Ok(measurements)
    }
}

impl TeslaApiClient {
    pub fn new() -> Self {
        Self {}
    }

    pub fn get_access_token(
        &self,
        config: &Config,
    ) -> Result<TeslaAccessToken, Box<dyn std::error::Error>> {
        info!("Fetching access token...");
        let url = "https://auth.tesla.com/oauth2/v3/token";

        debug!("POST {}", url);

        let request_body: TeslaAccessTokenRequest = TeslaAccessTokenRequest {
            grant_type: "refresh_token".into(),
            scope: "openid email offline_access".into(),
            client_id: "ownerapi".into(),
            refresh_token: config.refresh_token.clone(),
        };

        let access_token: TeslaAccessToken = retry(
            Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                .map(jitter)
                .take(RETRY_TAKES),
            || {
                reqwest::blocking::Client::new()
                    .post(url)
                    .json(&request_body)
                    .send()
            },
        )?
        .json()?;

        Ok(access_token)
    }

    pub fn get_vehicles(
        &self,
        token: &TeslaAccessToken,
    ) -> Result<Vec<TeslaVehicle>, Box<dyn std::error::Error>> {
        info!("Fetching vehicles...");
        let url = "https://owner-api.teslamotors.com/api/1/vehicles";

        debug!("GET {}", url);

        let vehicles_response: TeslaApiResponse<Vec<TeslaVehicle>> = retry(
            Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                .map(jitter)
                .take(RETRY_TAKES),
            || {
                reqwest::blocking::Client::new()
                    .get(url)
                    .bearer_auth(token.access_token.clone())
                    .send()
            },
        )?
        .json()?;

        Ok(vehicles_response.response)
    }

    pub fn get_vehicle_charge_state(
        &self,
        token: &TeslaAccessToken,
        vehicle: &TeslaVehicle,
    ) -> Result<TeslaVehicleChargeState, Box<dyn std::error::Error>> {
        info!(
            "Fetching vehicle charge state for {}...",
            vehicle.display_name
        );
        let url = format!(
            "https://owner-api.teslamotors.com/api/1/vehicles/{}/data_request/charge_state",
            vehicle.id
        );

        debug!("GET {}", url);

        let vehicle_data_response: TeslaApiResponse<TeslaVehicleChargeState> = retry(
            Exponential::from_millis_with_factor(RETRY_INTERVAL_MS, RETRY_FACTOR)
                .map(jitter)
                .take(RETRY_TAKES),
            || {
                reqwest::blocking::Client::new()
                    .get(&url)
                    .bearer_auth(token.access_token.clone())
                    .send()
            },
        )?
        .json()?;

        Ok(vehicle_data_response.response)
    }

    pub fn get_streaming_data(
        &self,
        token: &TeslaAccessToken,
        vehicle: &TeslaVehicle,
    ) -> Result<TeslaVehicleStreamingData, Box<dyn Error>> {
        info!(
            "Connecting to streaming api for vehicle {}",
            vehicle.display_name
        );

        let (mut socket, response) =
            connect(Url::parse("wss://streaming.vn.teslamotors.com/streaming/")?)?;

        debug!("Connected to the server");
        debug!("Response HTTP code: {}", response.status());

        let subscribe_message = TeslaStreamingApiMessage {
            msg_type: "data:subscribe_oauth".into(),
            tag: vehicle.vehicle_id.to_string(),
            token: Some(token.access_token.clone()),
            value: "speed,odometer,soc,elevation,est_heading,est_lat,est_lng,power,shift_state,range,est_range,heading".into(),
        };

        socket.write_message(Message::Text(serde_json::to_string(&subscribe_message)?))?;
        let start = Instant::now();
        loop {
            if start.elapsed().as_secs() > 30 {
                return Err(Box::<dyn Error>::from("Timed out after 30 seconds"));
            };

            let msg = socket.read_message()?;
            debug!("Received: {}", msg);

            if msg.is_close() {
                return Err(Box::<dyn Error>::from("Received close message"));
            }

            if !msg.is_binary() {
                debug!("Message is not of type binary, skipping");
                continue;
            }

            let msg_data = msg.into_data();

            let msg_value: serde_json::Value = serde_json::from_slice(&msg_data)?;
            if let Value::String(msg_type) = &msg_value["msg_type"] {
                match msg_type.as_str() {
                    "data:update" => {
                        let data_update_message: TeslaStreamingApiMessage =
                            serde_json::from_slice(&msg_data)?;

                        if data_update_message.tag != vehicle.vehicle_id.to_string() {
                            warn!("Receiving data for another vehicle");
                            continue;
                        }

                        let values: Vec<String> = data_update_message
                            .value
                            .split(',')
                            .map(str::to_string)
                            .collect();

                        if values.len() != 13 {
                            warn!("Receiving incorrect number of values");
                            continue;
                        }

                        let speed = values
                            .get(1)
                            .unwrap_or(&"0.0".to_string())
                            .parse()
                            .unwrap_or(0.0);

                        return Ok(TeslaVehicleStreamingData {
                            latitude: values
                                .get(6)
                                .unwrap_or(&"0.0".to_string())
                                .parse()
                                .unwrap_or(0.0),
                            longitude: values
                                .get(7)
                                .unwrap_or(&"0.0".to_string())
                                .parse()
                                .unwrap_or(0.0),
                            charger_power: if speed == 0.0 {
                                values
                                    .get(8)
                                    .unwrap_or(&"0.0".to_string())
                                    .parse::<f64>()
                                    .unwrap_or(0.0)
                                    .abs()
                            } else {
                                0.0
                            },
                            odometer: values
                                .get(2)
                                .unwrap_or(&"0.0".to_string())
                                .parse()
                                .unwrap_or(0.0),
                        });
                    }
                    "data:error" => {
                        return Err(Box::<dyn Error>::from(format!(
                            "Received error message: {}",
                            &msg_value["error_type"]
                        )));
                    }
                    _ => {
                        debug!("Unhandled message type {}", msg_type);
                    }
                }
            }
        }
    }

    pub fn get_last_values(
        &self,
        last_measurements: Option<Vec<Measurement>>,
        vehicle: &TeslaVehicle,
    ) -> (String, f64, f64, f64) {
        let last_measurement: Option<&Measurement> =
            if let Some(last_measurements) = &last_measurements {
                last_measurements.iter().find(|lm| {
                    lm.samples
                        .iter()
                        .any(|s| s.sample_name == vehicle.display_name)
                })
            } else {
                None
            };

        let last_charger_power: f64 = if let Some(last_measurement) = last_measurement {
            last_measurement
                .samples
                .iter()
                .find(|s| {
                    s.entity_type == EntityType::Device
                        && s.sample_type == SampleType::ElectricityConsumption
                        && s.sample_name == vehicle.display_name
                        && s.metric_type == MetricType::Gauge
                })
                .map(|s| s.value)
                .unwrap_or(0.0)
        } else {
            0.0
        };

        let last_charge_energy_added: f64 = if let Some(last_measurement) = last_measurement {
            last_measurement
                .samples
                .iter()
                .find(|s| {
                    s.entity_type == EntityType::Device
                        && s.sample_type == SampleType::ElectricityConsumption
                        && s.sample_name == vehicle.display_name
                        && s.metric_type == MetricType::Counter
                })
                .map(|s| s.value)
                .unwrap_or(0.0)
        } else {
            0.0
        };

        let last_odometer: f64 = if let Some(last_measurement) = last_measurement {
            last_measurement
                .samples
                .iter()
                .find(|s| {
                    s.entity_type == EntityType::Device
                        && s.sample_type == SampleType::DistanceTraveled
                        && s.sample_name == vehicle.display_name
                        && s.metric_type == MetricType::Counter
                })
                .map(|s| s.value)
                .unwrap_or(0.0)
        } else {
            0.0
        };

        let last_location = if let Some(last_measurement) = last_measurement {
            last_measurement.location.clone()
        } else {
            "Other".to_string()
        };

        (
            last_location,
            last_charger_power,
            last_charge_energy_added,
            last_odometer,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::model::GeofenceConfig;

    use super::*;

    #[test]
    #[ignore]
    fn get_vehicle_charge_state() {
        let tesla_api_client = TeslaApiClient::new();

        let refresh_token = env::var("TESLA_AUTH_REFRESH_TOKEN")
            .expect("Environment variable TESLA_AUTH_REFRESH_TOKEN not set");

        let config: Config = Config {
            refresh_token: refresh_token,
            geofences: vec![GeofenceConfig {
                location: "My Home".into(),
                latitude: 0.0,
                longitude: 0.0,
                geofence_radius_meters: 100.0,
            }],
        };

        // act
        let token = tesla_api_client
            .get_access_token(&config)
            .expect("Failed getting access token");

        let vehicles = tesla_api_client
            .get_vehicles(&token)
            .expect("Failed retrieving vehicles");

        for vehicle in vehicles {
            let vehicle_charge_state = tesla_api_client
                .get_vehicle_charge_state(&token, &vehicle)
                .expect("Failed getting vehicle charge state");

            debug!("{:?}", vehicle_charge_state);
        }
    }

    #[test]
    #[ignore]
    fn get_streaming_data() {
        let tesla_api_client = TeslaApiClient::new();

        let refresh_token = env::var("TESLA_AUTH_REFRESH_TOKEN")
            .expect("Environment variable TESLA_AUTH_REFRESH_TOKEN not set");

        let config: Config = Config {
            refresh_token: refresh_token,
            geofences: vec![GeofenceConfig {
                location: "My Home".into(),
                latitude: 0.0,
                longitude: 0.0,
                geofence_radius_meters: 100.0,
            }],
        };

        // act
        let token = tesla_api_client
            .get_access_token(&config)
            .expect("Failed getting access token");

        let vehicles = tesla_api_client
            .get_vehicles(&token)
            .expect("Failed retrieving vehicles");

        for vehicle in vehicles {
            let vehicle_charge_state = tesla_api_client
                .get_streaming_data(&token, &vehicle)
                .expect("Failed getting vehicle charge state");

            debug!("{:?}", vehicle_charge_state);
        }
    }
}
