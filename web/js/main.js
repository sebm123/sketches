export class MapContainer {
  constructor(elem) {
    this.segments = [];
    this.pendingReq = null;
    this.lastPoint = null;

    this.map = new mapboxgl.Map({
      container: elem,
      style: 'https://tiles.stadiamaps.com/styles/outdoors.json',
      center: [12, 53],
      zoom: 4,
      hash: true,
    });

    // Add zoom and rotation controls to the map.
    this.map.addControl(new mapboxgl.NavigationControl());
    this.map.addControl(new UndoMapControl(this));
    this.map.on('load', () => this.attachSources());
    this.map.on('click', (e) => this.handleClick(e));
  }

  attachSources() {
    this.map.addSource('points', {
      type: 'geojson',
      data: {
        type: 'Feature',
        properties: {},
        geometry: {
          type: 'LineString',
          coordinates: []
        }
      }
    });

    this.map.addSource('points-pending', {
      type: 'geojson',
      data: {
        type: 'Feature',
        properties: {},
        geometry: {
          type: 'LineString',
          coordinates: []
        }
      }
    });

    this.map.addLayer({
      id: 'route-outline',
      type: 'line',
      source: 'points',
      layout: {'line-join': 'round', 'line-cap': 'round'},
      paint: {'line-color': '#fff', 'line-width': 8}
    });

    this.map.addLayer({
      id: 'route',
      type: 'line',
      source: 'points',
      layout: {'line-join': 'round', 'line-cap': 'round'},
      paint: {'line-color': '#a3f', 'line-width': 3}
    });

    this.map.addLayer({
      id: 'route-pending',
      type: 'line',
      source: 'points-pending',
      layout: {'line-join': 'round', 'line-cap': 'round'},
      paint: {'line-color': '#fa3', 'line-width': 3}
    });
  }

  addLineSegment(points) {
    this.segments.push(points);
    this.redraw();
  }

  redraw() {
    const route = asLineString(
      this.segments
        .filter(it => !it.loading)
        .map(it => it.geometry)
        .flat()
    );
    const pending = asLineString(
      this.segments
        .filter(it => it.loading)
        .map(it => it.geometry)
        .flat()
    );

    this.map.getSource('points').setData(route);
    this.map.getSource('points-pending').setData(pending);
  }

  handleClick(e) {
    // It's too annoying to have multiple (fallible) requests in flight at once.
    if (this.pendingReq !== null) {
      return;
    }

    const point = {
      lat: e.lngLat.lat,
      lon: e.lngLat.lng,
    };

    let marker = new mapboxgl.Marker()
        .setLngLat([point.lon, point.lat])
        .addTo(this.map);

    if (this.lastPoint !== null) {
      let segment = {
        loading: true,
        from: this.lastPoint,
        to: point,
        geometry: [this.lastPoint, point],
        marker: marker,
      };

      this.pendingReq = Promise.resolve().then(() => {
        this.segments.push(segment);
        this.redraw();

        return fetch('/route', {
          headers: { 'Content-Type': 'application/json' },
          method: 'POST',
          body: JSON.stringify({
            from: this.lastPoint,
            to: point,
          }),
        });
      })
        .then(res => res.json())
        .then(res => {
          console.log('routing response', res);

          if (res === null) {
            this.popSegment();
            return;
          }

          this.lastPoint = res.geometry[res.geometry.length - 1];

          segment.loading = false;
          segment.geometry = res.geometry;
          segment.marker
            .setLngLat(this.lastPoint)
            .setPopup(new mapboxgl.Popup().setHTML(`
                  <ul>
                    <li>distance: ${res.distance_meters} meters</li>
                    <li>cost    : ${res.route_cost}</li>
                  </ul>`));
          this.redraw();
        })
        .catch(err => {
          console.error('something went wrong', err);
          this.popSegment();
        })
        .finally(() => {
          this.pendingReq = null;
        });

    } else {
      this.lastPoint = point;
    }
  }

  popSegment() {
    if (this.segments.length === 0) { return; }

    const segment = this.segments.pop();
    segment.marker.remove();
    this.lastPoint = segment.from;

    this.redraw();
  }
}

function asLineString(points) {
  return {
    type: 'Feature',
    properties: {},
    geometry: {
      type: 'LineString',
      coordinates: points.map(({lat, lon}) => [lon, lat])
    }
  };
}

class UndoMapControl {
  constructor(container) {
    this._container = container;
  }

  onAdd(map) {
    this._map = map;
    this._elem = document.createElement('button');
    this._elem.className = 'mapboxgl-ctrl';
    this._elem.textContent = '🔙';
    this._elem.onclick = () => this.onClick();
    return this._elem;
  }

  onClick() {
    this._container.popSegment();
  }


  onRemove() {
    this._elem.parentNode.removeChild(this._elem);
    this._map = undefined;
  }
}
