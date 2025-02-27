package main

import (
	"encoding/binary"
	"flag"
	"fmt"
	"strings"
	"sync"

	"tinygo.org/x/bluetooth"
)

var KnownServiceUUIDs = []bluetooth.UUID{
	bluetooth.ServiceUUIDCyclingSpeedAndCadence,
	bluetooth.ServiceUUIDCyclingPower,
	bluetooth.ServiceUUIDHeartRate,

	// General controllable device, seems more involved.
	// bluetooth.ServiceUUIDFitnessMachine,
}

var KnownServiceCharacteristicUUIDs = map[bluetooth.UUID][]bluetooth.UUID{
	// https://www.bluetooth.com/specifications/specs/cycling-power-service-1-1/
	bluetooth.ServiceUUIDCyclingPower: {
		bluetooth.CharacteristicUUIDCyclingPowerMeasurement,
		// TODO:
		// Not a standardized characteristic, but this is offered by KICKR.
		// See GoldenCheetah source for some use examples:
		// https://github.com/GoldenCheetah/GoldenCheetah/blob/master/src/Train/BT40Device.cpp
		//
		// var WahooKickrControlCharacteristic = bluetooth.ParseUUID(
		// 	"a026e005-0a7d-4ab3-97fa-f1500f9feb8b"
		// )
	},
	bluetooth.ServiceUUIDHeartRate: {
		bluetooth.CharacteristicUUIDHeartRateMeasurement,
	},
}
var (
	KnownServiceNames = map[bluetooth.UUID]string{
		bluetooth.ServiceUUIDCyclingPower: "Cycling Power",
		bluetooth.ServiceUUIDHeartRate:    "Heart Rate",
		// TODO: bluetooth.ServiceUUIDCyclingSpeedAndCadence: "Cycling Speed and Cadence",
	}
	KnownCharacteristicNames = map[bluetooth.UUID]string{
		bluetooth.CharacteristicUUIDCyclingPowerMeasurement: "Cycling Power Measure",
		bluetooth.CharacteristicUUIDHeartRateMeasurement:    "Heart Rate Measurement",
		// TODO: bluetooth.CharacteristicUUIDCSCMeasurement:          "Cycling Speed and Cadence Measurement",
	}
)

type MetricKind int

const (
	MetricHeartRate MetricKind = iota
	MetricCyclingPower
	MetricCyclingSpeed
	MetricCyclingCadence
)

type DeviceMetric struct {
	kind  MetricKind
	value int
}

type MetricSource struct {
	sinks []chan DeviceMetric

	svc *bluetooth.DeviceService
	ch  *bluetooth.DeviceCharacteristic
}

func NewMetricSource(
	svc *bluetooth.DeviceService,
	ch *bluetooth.DeviceCharacteristic,
) MetricSource {
	return MetricSource{
		sinks: []chan DeviceMetric{},
		svc:   svc,
		ch:    ch,
	}
}

func (src *MetricSource) Name() string {
	if name, ok := KnownCharacteristicNames[src.ch.UUID()]; ok {
		return name
	}
	return fmt.Sprintf("<unknown: %s>", src.ch.UUID().String())
}

func (src *MetricSource) AddSink(sink chan DeviceMetric) {
	src.sinks = append(src.sinks, sink)

	// Start listenening first time we add a sink
	if len(src.sinks) == 1 {
		handler := src.notificationHandler()
		src.ch.EnableNotifications(handler)
	}
}

func (src *MetricSource) notificationHandler() func([]byte) {
	switch src.ch.UUID() {
	case bluetooth.CharacteristicUUIDCyclingPowerMeasurement:
		return src.handleCyclingPowerMeasurement

	case bluetooth.CharacteristicUUIDHeartRateMeasurement:
		return src.handleHeartRateMeasurement

	// TODO: Add these
	// case bluetooth.CharacteristicUUIDCSCMeasurement:
	// 	return src.handleSpeedCadenceMeasurement

	default:
		println("BUG: missing notification handler:", src.ch.UUID().String())
	}

	return nil
}

func (src *MetricSource) emit(m DeviceMetric) {
	for _, sink := range src.sinks {
		sink <- m
	}
}

const (
	// BPM size, 0 if u8, 1 if u16
	HeartRateFlagSize = 1 << 0

	// 00 unsupported
	// 01 unsupported
	// 10 supported, not detected
	// 11 supported, detected
	HeartRateFlagContactStatus = (1 << 1) | (1 << 2)

	HeartRateFlagHasEnergyExpended = 1 << 3
	HeartRateFlagHasRRInterval     = 1 << 4

	// bits 5-8 reserved
)

func (src *MetricSource) handleHeartRateMeasurement(buf []byte) {
	// malformed
	if len(buf) < 2 {
		return
	}

	flag := buf[0]

	is16Bit := (flag & HeartRateFlagSize) != 0
	contactStatus := (flag & HeartRateFlagContactStatus) >> 1

	contactSupported := contactStatus&(0b10) != 0
	contactFound := contactStatus&(0b01) != 0

	// No use sending this metric if the sensor isn't reading.
	if contactSupported && !contactFound {
		return
	}

	var hr int = int(buf[1])
	if is16Bit {
		hr = int(int16(binary.LittleEndian.Uint16(buf[1:])))
	}

	src.emit(DeviceMetric{
		kind:  MetricHeartRate,
		value: hr,
	})
}

const (
	CyclingPowerFlagHasPedalPowerBalance           = 1 << 0
	CyclingPowerFlagPedalPowerBalanceReference     = 1 << 1
	CyclingPowerFlagHasAccumulatedTorque           = 1 << 2
	CyclingPowerFlagAccumulatedTorqueSource        = 1 << 3
	CyclingPowerFlagHasWheelRevolution             = 1 << 4
	CyclingPowerFlagHasCrankRevolution             = 1 << 5
	CyclingPowerFlagHasExtremeForceMagnitudes      = 1 << 6
	CyclingPowerFlagHasExtremeTorqueMagnitudes     = 1 << 7
	CyclingPowerFlagHasExtremeAngles               = 1 << 8
	CyclingPowerFlagHasTopDeadSpotAngle            = 1 << 9
	CyclingPowerFlagHasBottomDeadSpotAngle         = 1 << 10
	CyclingPowerFlagHasAccumulatedEnergy           = 1 << 11
	CyclingPowerFlagHasOffsetCompensationIndicator = 1 << 12

	// Bits 13-16 reserved
)

// Two flag bytes, followed by a 16 bit power reading. All subsequent
// fields are optional, based on the flag bits set.
//
// sint16  instantaneous_power      watts with resolution 1
// uint8   pedal_power_balance      percentage with resolution 1/2
// uint16  accumulated_torque       newton meters with resolution 1/32
// uint32  wheel_rev_cumulative     unitless
// uint16  wheel_rev_last_time      seconds with resolution 1/2048
// uint16  crank_rev_cumulative     unitless
// uint16  crank_rev_last_time      seconds with resolution 1/1024
// sint16  extreme_force_max_magn   newtons with resolution 1
// sint16  extreme_force_min_magn   newtons with resolution 1
// sint16  extreme_torque_max_magn  newton meters with resolution 1/32
// sint16  extreme_torque_min_magn  newton meters with resolution 1/32
// uint12  extreme_angles_max       degrees with resolution 1
// uint12  extreme_angles_min       degrees with resolution 1
// uint16  top_dead_spot_angle      degrees with resolution 1
// uint16  bottom_dead_spot_angle   degrees with resolution 1
// uint16  accumulated_energy       kilojoules with resolution 1
func (src *MetricSource) handleCyclingPowerMeasurement(buf []byte) {
	// malformed
	if len(buf) < 2 {
		return
	}

	flags := binary.LittleEndian.Uint16(buf[0:])
	powerWatts := int16(binary.LittleEndian.Uint16(buf[2:]))

	// Power meters will send packets even if nothing's happening.
	if powerWatts == 0 {
		return
	}
	src.emit(DeviceMetric{
		kind:  MetricCyclingPower,
		value: int(powerWatts),
	})

	// These fields are optional, so we need to index over them, can't skip directly.
	offset := 4
	if flags&CyclingPowerFlagHasPedalPowerBalance != 0 {
		offset += 1
	}
	if flags&CyclingPowerFlagHasAccumulatedTorque != 0 {
		offset += 2
	}

	// TODO: Calculate speed from this
	if flags&CyclingPowerFlagHasWheelRevolution != 0 {
		// rev := binary.LittleEndian.Uint32(buf[offset:])
		// time := binary.LittleEndian.Uint16(buf[offset+4:])

		offset += 4 + 2
	}

	// TODO: Calculate cadence from this
	if flags&CyclingPowerFlagHasCrankRevolution != 0 {
		// rev := binary.LittleEndian.Uint16(buf[offset:])
		// time := binary.LittleEndian.Uint16(buf[offset+2:])

		offset += 2 + 2
	}

}

func scanDevices() {
	adapter := bluetooth.DefaultAdapter
	fmt.Println("Starting device scan...")

	if err := adapter.Enable(); err != nil {
		fmt.Println("FATAL: Failed to enable BLE")
		panic(err)
	}

	// Keep track of addresses we've already looked ad
	addrsChecked := map[string]bool{}

	onScanResult := func(bt *bluetooth.Adapter, result bluetooth.ScanResult) {
		if _, seen := addrsChecked[result.Address.String()]; seen {
			return
		}
		addrsChecked[result.Address.String()] = true

		serviceNames := []string{}
		for _, s := range KnownServiceUUIDs {
			if !result.HasServiceUUID(s) {
				continue
			}

			serviceNames = append(serviceNames, KnownServiceNames[s])
		}

		// No matching services, skip this device.
		if len(serviceNames) == 0 {
			return
		}

		fmt.Printf("%s %-20s %-20s [RSSI:%d]\n",
			result.Address.String(),
			result.LocalName(),
			strings.Join(serviceNames, ","),
			result.RSSI,
		)
	}

	if err := adapter.Scan(onScanResult); err != nil {
		fmt.Println("FATAL: Failed to scan for devices")
		panic(err)
	}

	fmt.Println("Scan complete.")
}

type repeatableFlag []string

func (i *repeatableFlag) String() string {
	return strings.Join(*i, ",")
}

func (i *repeatableFlag) Set(value string) error {
	*i = append(*i, value)
	return nil
}

var (
	flagScanMode    bool
	flagDeviceAddrs repeatableFlag
)

func init() {
	flag.BoolVar(&flagScanMode, "scan", false, "scan for nearby devices")
	flag.Var(&flagDeviceAddrs, "device", "BLE device address")

	flag.Parse()
}

func main() {
	if flagScanMode {
		scanDevices()
		return
	}

	adapter := bluetooth.DefaultAdapter
	if err := adapter.Enable(); err != nil {
		fmt.Println("FATAL: Failed to enable BLE")
		panic(err)
	}

	deviceChan := make(chan *bluetooth.Device)

	wg := sync.WaitGroup{}

	connectRetry := func(addr string) {
		println("starting connection attempt for", addr)
		uuid, err := bluetooth.ParseUUID(addr)
		if err != nil {
			fmt.Printf("FATAL: bad UUID given: <%s>\n", addr)
			panic(err)
		}

		// NOTE: ConnectionTimeout is ignored on Mac OS
		params := bluetooth.ConnectionParams{}

		// TODO: We should add a time bound for this
		for {
			// TODO: bluetooth.Address bit is not cross-platform.
			device, err := adapter.Connect(bluetooth.Address{uuid}, params)
			if err != nil {
				println("device timed out:", uuid.String())
				continue
			}

			println("device found:", uuid.String())
			deviceChan <- device
			break
		}

		wg.Done()
	}

	for _, addr := range flagDeviceAddrs {
		wg.Add(1)
		go connectRetry(addr)
	}

	go func() {
		wg.Wait()
		close(deviceChan)
	}()

	metricsChan := make(chan DeviceMetric)
	go func() {
		for m := range metricsChan {
			fmt.Printf("Metric: %+v\n", m)
		}
	}()

	for device := range deviceChan {
		fmt.Println("Initializing device...")
		services, err := device.DiscoverServices(KnownServiceUUIDs)
		if err != nil {
			panic(err)
		}

		for _, service := range services {
			if name, ok := KnownServiceNames[service.UUID()]; ok {
				fmt.Printf("\tservice: %s\n", name)
			} else {
				fmt.Printf("\tservice: unknown <%+v>\n", service.UUID().String())
			}

			knownChars := KnownServiceCharacteristicUUIDs[service.UUID()]
			chars, err := service.DiscoverCharacteristics(knownChars)
			if err != nil {
				panic(err)
			}

			for _, char := range chars {
				name := KnownCharacteristicNames[char.UUID()]
				fmt.Printf("\t\tcharacteristic: %s\n", name)

				src := NewMetricSource(&service, &char)
				src.AddSink(metricsChan)
			}
		}
	}

	println("that's all!")
	select {}
}
